import assert from "node:assert/strict";
import { test } from "node:test";
import { PublicKey } from "@solana/web3.js";
import { projectIncomeClaim, projectHolderIncome, incomeArchiveRequests, readIncomeArchives, deriveIncomeEpochAddress } from "../dist/index.js";
const Q=1n<<64n;
const zero=()=>({unitsQ64:0n,weightedPaidQ128:[0n,0n,0n,0n]});
const growth=(units,paid=0n)=>({unitsQ64:units,weightedPaidQ128:[units*paid& (Q-1n),units*paid>>64n& (Q-1n),units*paid>>128n& (Q-1n),units*paid>>192n]});
const claim=(overrides={})=>({epoch:0n,unitsQ64:0n,paidCheckpointQ64:0n,growthCheckpoint:zero(),paidFractionQ128:0n,claimable:0n,...overrides});
const book=(overrides={})=>({epoch:0n,unpaid:0n,units:0n,paidPerUnitQ64:0n,growth:[zero(),zero(),zero(),zero()],...overrides});
const holder=(first=claim(),overrides={})=>({tokenKind:0,incomeInitialized:true,escrowBalance:0n,income:[first,claim()],...overrides});

test("funded old units stay claimable when their owner's LP balance is zero",()=>{
 const h=holder(claim({unitsQ64:10n*Q}));const books=[book({paidPerUnitQ64:Q/2n}),book(),book(),book()];
 assert.equal(projectHolderIncome({holder:h,books,asset:0,walletBalance:0n}),5n);
 assert.equal(h.income[0].paidCheckpointQ64,0n,"projection must not mutate the account");
});
test("late entrants never receive payments made before their units were issued",()=>{
 const b=book({paidPerUnitQ64:Q,growth:[growth(Q,Q/2n),zero(),zero(),zero()]});
 assert.equal(projectIncomeClaim({claim:claim(),book:b,balance:4n,lane:0}).claimable,2n);
});
test("fractional cash survives repeated checkpoints exactly",()=>{
 const c=claim({unitsQ64:Q});const first=projectIncomeClaim({claim:c,book:book({paidPerUnitQ64:Q/2n}),balance:0n,lane:0});
 assert.equal(first.claimable,0n);assert.equal(first.paidFractionQ128,1n<<127n);
 assert.equal(projectIncomeClaim({claim:first,book:book({paidPerUnitQ64:Q}),balance:0n,lane:0}).claimable,1n);
});
test("junior escrow earns income and retired generations stop acquiring replacement units",()=>{
 const h=holder(claim(),{tokenKind:2,escrowBalance:2n});
 const b=book({paidPerUnitQ64:Q,growth:[zero(),growth(100n*Q),zero(),zero()]});
 const ends=[{ended:true,epoch:0n,growth:growth(Q)},...Array.from({length:3},()=>({ended:false,epoch:0n,growth:zero()}))];
 assert.equal(projectHolderIncome({holder:h,books:[b,book(),book(),book()],asset:0,walletBalance:3n,ends}),5n);
});
test("archived denominations carry old ownership and collected cash forward",()=>{
 const h=holder(claim({unitsQ64:10n*Q}));
 const old=book({units:10n,unpaid:5n,paidPerUnitQ64:Q/2n});
 const books=[book({epoch:1n,units:5n,paidPerUnitQ64:Q}),book(),book(),book()];
 assert.deepEqual(incomeArchiveRequests(h,books,0),[{bookIndex:0,epoch:0n}]);
 assert.throws(()=>projectHolderIncome({holder:h,books,asset:0,walletBalance:0n}),/IncomeHistoryRequired/);
 assert.equal(projectHolderIncome({holder:h,books,asset:0,walletBalance:0n,archives:new Map([["0:0",old]])}),10n);
});
test("uninitialized holders acquire no historical units; regression and overflow fail closed",()=>{
 const books=[book({paidPerUnitQ64:Q,growth:[growth(Q),zero(),zero(),zero()]}),book(),book(),book()];
 assert.equal(projectHolderIncome({holder:holder(claim(),{incomeInitialized:false}),books,asset:0,walletBalance:100n}),0n);
 assert.throws(()=>projectIncomeClaim({claim:claim({growthCheckpoint:growth(2n*Q)}),book:books[0],balance:1n,lane:0}),/regressed/);
 assert.throws(()=>projectIncomeClaim({claim:claim({claimable:Q-1n,unitsQ64:Q}),book:books[0],balance:0n,lane:0}),/overflow/);
 assert.throws(()=>projectIncomeClaim({claim:claim(),book:book({growth:[growth(Q,Q),zero(),zero(),zero()]}),balance:1n,lane:0}),/weighted/);
 assert.throws(()=>incomeArchiveRequests(holder(),[book({epoch:33n}),book(),book(),book()],0),/IncomeHistoryRequired/);
});
test("archive reads authenticate the market, epoch, lane, owner, PDA bump and bank floor",async()=>{
 const market=new PublicKey(new Uint8Array(32).fill(9));const program=new PublicKey(new Uint8Array(32).fill(10));
 const [key,bump]=deriveIncomeEpochAddress(market,0,0n,program);
 const base={market,bookIndex:0,book:book(),bump};let decoded=base;let accountOwner=program;let slot=50;
 const dusk={program:{programId:program,coder:{accounts:{decode:()=>decoded}},provider:{connection:{getMultipleAccountsInfoAndContext:async(keys,options)=>{assert.equal(keys[0].toBase58(),key.toBase58());assert.equal(options.minContextSlot,50);return {context:{slot},value:[{owner:accountOwner,executable:false,data:Buffer.alloc(1)}]}}}}}};
 const input={dusk,market,holder:holder(),books:[book({epoch:1n}),book(),book(),book()],asset:0,minContextSlot:50};
 assert.equal((await readIncomeArchives(input)).size,1);
 for(const patch of [{market:PublicKey.default},{bookIndex:1},{book:book({epoch:1n})},{bump:bump^1}]){decoded={...base,...patch};await assert.rejects(readIncomeArchives(input),/Invalid income archive/)}
 decoded=base;accountOwner=PublicKey.default;await assert.rejects(readIncomeArchives(input),/IncomeHistoryRequired/);
 accountOwner=program;slot=49;await assert.rejects(readIncomeArchives(input),/stale/);
});


test("matches the program's transfer, partial-writeoff and retired-epoch vectors",()=>{
 const paid=80n*Q/100n;
 const first=book({units:100n,unpaid:0n,paidPerUnitQ64:paid,growth:[growth(Q),zero(),zero(),zero()]});
 const alice=projectIncomeClaim({claim:claim(),book:book({...first,paidPerUnitQ64:0n}),balance:100n,lane:0});
 const bob=projectIncomeClaim({claim:claim(),book:book({...first,paidPerUnitQ64:0n}),balance:0n,lane:0});
 const settled=projectIncomeClaim({claim:alice,book:first,balance:0n,lane:0});
 assert.equal(settled.claimable,79n);
 assert.ok(settled.paidFractionQ128>0n);
 assert.equal(projectIncomeClaim({claim:bob,book:first,balance:100n,lane:0}).claimable,0n);
 const old=book({units:100n,unpaid:50n,growth:[growth(Q),growth(Q),zero(),zero()]});
 const after=book({epoch:1n,units:50n,paidPerUnitQ64:Q});
 const junior=holder(claim(),{tokenKind:2});
 const ends=Array.from({length:4},()=>({ended:true,epoch:0n,growth:growth(Q)}));
 assert.equal(projectHolderIncome({holder:junior,books:[after,book(),book(),book()],asset:0,walletBalance:20n,ends,archives:new Map([["0:0",old]])}),10n);
});
