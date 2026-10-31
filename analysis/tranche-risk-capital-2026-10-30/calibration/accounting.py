"""Exact two-token funded ledger and paged historical-income prototype.

Not native Solana code. Historical share checkpoints and per-loan income records
consume storage. Loan accrual must be prepared through the relevant ownership/rate
epochs before repayment; that catch-up is explicitly bounded per call, not free.
"""
from bisect import bisect_right
from collections import defaultdict, deque
from dataclasses import dataclass, field
from fractions import Fraction as F


class History:
    def __init__(self):
        self.seq=0
        self.balances=defaultdict(list)
        self.supply={}

    def change(self, who, token, balance, supply):
        self.seq+=1
        self.balances[who,token].append((self.seq,F(balance)))
        self.supply[token]=F(supply)

    def at(self, who, token, seq):
        records=self.balances[who,token]
        i=bisect_right(records,(seq,F(10**100)))-1
        return records[i][1] if i>=0 else F(0)


class Capital:
    def __init__(self):
        self.price=[F(2),F(1)]
        self.cash=[F(0),F(0)]
        self.principal=[F(0),F(0)]
        self.insurance=[F(0),F(0)]
        self.nav={"sr":F(0),"jr":F(0)}
        self.epoch=0
        self.tokens={"sr":"sr:0","jr":"jr:0"}
        self.units=defaultdict(F)
        self.locked=defaultdict(F)
        self.supply=defaultdict(F)
        self.history=History()

    def shares(self, who, tier):
        return self.units[who,self.tokens[tier]]

    def mint_value(self, who, tier, value):
        value=F(value)
        if tier=="jr" and self.nav[tier]==0 and self.supply[self.tokens[tier]]:
            # Old receipts remain associated with the closed generation.
            self.epoch+=1
            self.tokens[tier]=f"jr:{self.epoch}"
        token=self.tokens[tier]
        units=value*self.supply[token]/self.nav[tier] if self.supply[token] else value
        self.units[who,token]+=units
        self.supply[token]+=units
        self.nav[tier]+=value
        self.history.change(who,token,self.units[who,token],self.supply[token])
        return units

    def burn(self, who, tier, units):
        assert who!="seed", "permanent seed cannot redeem or convert"
        token=self.tokens[tier]
        units=F(units)
        assert 0<units<=self.units[who,token]-self.locked[who,token]
        value=units*self.nav[tier]/self.supply[token]
        self.units[who,token]-=units
        self.supply[token]-=units
        self.nav[tier]-=value
        self.history.change(who,token,self.units[who,token],self.supply[token])
        return value

    def deposit(self, who, base, quote):
        self.cash[0]+=base
        self.cash[1]+=quote
        self.mint_value(who,"sr",base*self.price[0]+quote)
        self.check()

    def convert(self, who, source, units):
        value=self.burn(who,source,units)
        if value:
            self.mint_value(who,"jr" if source=="sr" else "sr",value)
        self.check()

    def seed_from_creator(self, creator, fraction):
        value=self.burn(creator,"sr",self.shares(creator,"sr")*fraction)
        self.mint_value("seed","jr",value)
        self.check()

    def transfer(self, who, recipient, tier, units):
        assert who!="seed", "permanent seed cannot transfer"
        token=self.tokens[tier]
        assert 0<=units<=self.units[who,token]-self.locked[who,token]
        self.units[who,token]-=units
        self.units[recipient,token]+=units
        self.history.change(who,token,self.units[who,token],self.supply[token])
        self.history.change(recipient,token,self.units[recipient,token],self.supply[token])

    def compound_seed(self, asset, amount):
        self.cash[asset]+=amount
        self.mint_value("seed","jr",amount*self.price[asset])
        self.check()

    def lend(self, asset, amount):
        assert self.cash[asset]>=amount>=0
        self.cash[asset]-=amount
        self.principal[asset]+=amount
        self.check()

    def repay_principal(self, asset, amount):
        assert 0<=amount<=self.principal[asset]
        self.principal[asset]-=amount
        self.cash[asset]+=amount
        self.check()

    def retire_internal_collateral(self, who, sr_units, debt_asset):
        value=self.burn(who,"sr",sr_units)
        reduction=value/self.price[debt_asset]
        assert reduction<=self.principal[debt_asset]
        self.principal[debt_asset]-=reduction
        self.check()
        return reduction

    def loss(self, asset, amount, max_insurance):
        assert 0<=amount<=self.principal[asset]
        cover=min(F(amount),self.insurance[asset],F(max_insurance))
        self.insurance[asset]-=cover
        self.cash[asset]+=cover
        self.principal[asset]-=amount
        residual=(amount-cover)*self.price[asset]
        jr_loss=min(residual,self.nav["jr"])
        self.nav["jr"]-=jr_loss
        self.nav["sr"]-=residual-jr_loss
        self.check()
        return cover,jr_loss,residual-jr_loss

    def check(self):
        funded=sum(self.price[a]*(self.cash[a]+self.principal[a]) for a in (0,1))
        assert sum(self.nav.values())==funded
        assert all(x>=0 for x in self.cash+self.principal+self.insurance+list(self.nav.values()))


@dataclass
class IncomePool:
    asset: int
    unpaid: F=F(0)
    supply: F=F(0)
    paid_index: F=F(0)


@dataclass
class Record:
    pool: IncomePool
    units: F
    start_paid_index: F
    seq: int
    tokens: dict
    supplies: dict
    split: tuple
    seed_direct: bool
    excluded_sr: tuple=()
    claimed: dict=field(default_factory=dict)


class Income:
    MAX_PAGE=8

    def __init__(self, capital):
        self.capital=capital
        self.pools={}
        self.records=[]
        self.custody=[F(0),F(0)]
        self.payout=defaultdict(F)
        self.deferred=defaultdict(deque)

    def queue_accrual(self, loan, asset, amount, junior_only=False, excluded_sr=()):
        # In a native implementation these arguments are derived from verifiable
        # debt-index/ownership epochs, never accepted as trusted off-chain amounts.
        c=self.capital
        assert len(excluded_sr)<=2
        supplies={tier:c.supply[c.tokens[tier]] for tier in ("sr","jr")}
        supplies["sr"]-=sum((c.shares(who,"sr") for who in excluded_sr),F(0))
        self.deferred[loan].append((loan,asset,F(amount),c.history.seq,dict(c.tokens),
            supplies,(F(0),F(1)) if junior_only else (F(1,2),F(1,2)),
            c.nav["jr"]==0,tuple(excluded_sr)))

    def prepare_page(self, loan_id, count=8):
        assert 0<count<=self.MAX_PAGE
        work=[]
        for _ in range(min(count,len(self.deferred[loan_id]))):
            work.append(self.deferred[loan_id].popleft())
        for loan,asset,amount,seq,tokens,supplies,split,direct,excluded in work:
            pool=self.pools.get(loan)
            if pool is None or (pool.unpaid==0 and pool.supply>0):
                pool=IncomePool(asset)
                self.pools[loan]=pool
            assert pool.asset==asset
            units=amount*pool.supply/pool.unpaid if pool.supply else amount
            pool.unpaid+=amount
            pool.supply+=units
            self.records.append(Record(pool,units,pool.paid_index,seq,tokens,supplies,split,direct,excluded))
        return len(work)

    def collect(self, loan, amount):
        assert not self.deferred[loan], "prepare this loan's accrual pages before collection"
        pool=self.pools[loan]
        amount=F(amount)
        assert 0<amount<=pool.unpaid
        pool.paid_index+=amount/pool.supply
        pool.unpaid-=amount
        self.custody[pool.asset]+=amount

    def writeoff(self, loan, amount):
        assert not self.deferred[loan], "prepare this loan's accrual pages before writeoff"
        pool=self.pools[loan]
        assert 0<=amount<=pool.unpaid
        pool.unpaid-=amount

    def entitlement(self, record, who):
        fraction=F(0)
        for tier,weight in zip(("sr","jr"),record.split):
            if not weight:
                continue
            if tier=="sr" and who in record.excluded_sr:
                continue
            if tier=="jr" and record.seed_direct:
                fraction+=weight if who=="seed" else 0
            elif record.supplies[tier]:
                balance=self.capital.history.at(who,record.tokens[tier],record.seq)
                fraction+=weight*balance/record.supplies[tier]
            else:
                # Explicit analysis placeholder for unselected empty-senior policy.
                fraction+=weight if who=="reserve" else 0
        return record.units*(record.pool.paid_index-record.start_paid_index)*fraction

    def claim_page(self, who, ids):
        assert len(ids)<=self.MAX_PAGE and len(set(ids))==len(ids)
        amounts=[F(0),F(0)]
        for i in ids:
            record=self.records[i]
            cumulative=self.entitlement(record,who)
            due=cumulative-record.claimed.get(who,F(0))
            assert due>=0
            record.claimed[who]=cumulative
            amounts[record.pool.asset]+=due
        for asset,amount in enumerate(amounts):
            assert self.custody[asset]>=amount
            self.custody[asset]-=amount
            if who=="seed" and amount:
                self.capital.compound_seed(asset,amount)
            else:
                self.payout[who,asset]+=amount
        return amounts


class RestrictionClock:
    """Clock mechanics only. Activation must be derived from verified insolvency.

    Historical prefix records support bounded witness checks in a native design;
    this reference uses binary search to locate them. It does not solve detecting
    all insolvent positions or economic griefing by repeatedly creating them.
    """
    def __init__(self):
        self.intervals=[]  # [start, end, cumulative blocked time before this interval]
        self.starts=[]

    def begin(self, now):
        assert not self.intervals or self.intervals[-1][1] is not None
        assert not self.intervals or now>=self.intervals[-1][1]
        prior=self.blocked_until(now)
        self.intervals.append([now,None,prior])
        self.starts.append(now)

    def end(self, now):
        assert self.intervals and self.intervals[-1][1] is None
        assert now>=self.intervals[-1][0]
        self.intervals[-1][1]=now

    def blocked(self, now):
        return bool(self.intervals and self.intervals[-1][1] is None and self.intervals[-1][0]<=now)

    def blocked_until(self, now):
        # Python key-based bisect reads logarithmically many records; no list scan.
        i=bisect_right(self.starts,now)-1
        if i<0:
            return 0
        start,end,prior=self.intervals[i]
        return prior+max(0,min(now,end if end is not None else now)-start)

    def elapsed_available(self, start, now):
        return max(0,now-start)-(self.blocked_until(now)-self.blocked_until(start))


class Exits:
    def __init__(self, capital, fee_bps=0):
        self.c=capital
        self.fee_bps=F(fee_bps)
        self.requests={}
        self.pending=defaultdict(F)
        self.clock=RestrictionClock()

    def request(self, who, new_units, now):
        assert who!="seed"
        token=self.c.tokens["jr"]
        old=self.requests.get(who)
        assert old is None or old["token"]==token
        old_units=old["units"] if old else F(0)
        new_units=F(new_units)
        assert 0<=new_units<=self.c.units[who,token]-self.c.locked[who,token]
        total=old_units+new_units
        assert total>0
        fee_base=total if old and now>=old["ready"] else new_units
        fee=fee_base*self.fee_bps/10_000
        duration=7+3*min(F(1),(self.pending[token]+new_units)/self.c.supply[token])
        ready=max(now+duration,old["ready"] if old else now)
        self.c.locked[who,token]-=old_units
        if fee:
            self.c.transfer(who,"seed","jr",fee)
        total-=fee
        self.c.locked[who,token]+=total
        self.pending[token]+=total-old_units
        self.requests[who]={"units":total,"token":token,"ready":ready}
        return dict(self.requests[who],fee=fee)

    def cancel(self, who, now):
        q=self.requests[who]
        assert now>=q["ready"]
        self.c.locked[who,q["token"]]-=q["units"]
        self.pending[q["token"]]-=q["units"]
        del self.requests[who]

    def claim(self, who, now):
        q=self.requests[who]
        assert now>=q["ready"] and not self.clock.blocked(now)
        assert self.clock.elapsed_available(q["ready"],now)<=2
        units=q["units"]
        token=q["token"]
        self.cancel(who,now)
        if token==self.c.tokens["jr"] and self.c.nav["jr"]>0:
            self.c.convert(who,"jr",units)
