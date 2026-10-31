use super::*;

struct FeedAccount {key:Pubkey,owner:Pubkey,lamports:u64,data:Vec<u8>}
impl FeedAccount {
    fn info(&mut self)->AccountInfo<'_> {
        AccountInfo::new(&self.key,false,false,&mut self.lamports,&mut self.data,&self.owner,false,0)
    }
}
fn clock()->Clock {Clock {slot:100,unix_timestamp:1000,..Clock::default()}}
fn normalized()->(super::super::Market,NormalizedOracleV1,FeedAccount) {
    let mut market=super::super::Market::default();
    market.base_side.asset_mint=Pubkey::new_unique();market.quote_side.asset_mint=Pubkey::new_unique();
    let provider=Pubkey::new_unique();let account=Pubkey::new_unique();
    market.oracle.policy=OraclePolicy{format:NORMALIZED_ORACLE_V1,provider,base_account:account,
        base_feed_id:[7;32],lambda_bps:5000,adjustment_cap_nad:2*NAD,max_age_seconds:30,max_age_slots:20,
        max_confidence_bps:100,..OraclePolicy::default()};
    let feed=NormalizedOracleV1 {magic:NORMALIZED_ORACLE_MAGIC,feed_id:[7;32],base_mint:market.base_side.asset_mint,
        quote_mint:market.quote_side.asset_mint,price_nad:60*NAD,confidence_nad:NAD/10,publish_time:995,
        publish_slot:95,sequence:1,status:1};
    let fixture=FeedAccount{key:account,owner:provider,lamports:1,data:feed.try_to_vec().unwrap()};
    (market,feed,fixture)
}

#[test]
fn policy_limits_do_not_depend_on_market_creation_settings() {
    OraclePolicy::default().validate().unwrap();
    let (market,_,_)=normalized();
    for lambda in [0,1,2500,9999] {
        for cap in [NAD+1,2*NAD,3*NAD,u64::MAX] {
            let policy=OraclePolicy {lambda_bps:lambda,adjustment_cap_nad:cap,..market.oracle.policy};
            policy.validate().unwrap();
        }
    }
    for lambda in [10_000,u16::MAX] {
        assert!(OraclePolicy {lambda_bps:lambda,..market.oracle.policy}.validate().is_err());
    }
    for cap in [0,NAD-1,NAD] {
        assert!(OraclePolicy {adjustment_cap_nad:cap,..market.oracle.policy}.validate().is_err());
    }
    // Positive weight cannot turn an empty/no-source policy into an oracle.
    assert!(OraclePolicy {lambda_bps:1,adjustment_cap_nad:2*NAD,..Default::default()}.validate().is_err());
}

#[test]
fn guided_first_liquidity_uses_the_report_not_the_callers_deposit_ratio() {
    for (base_decimals, quote_decimals) in [(0,0),(6,6),(6,9),(12,6),(6,12)] {
        for price_nad in [NAD / 3, NAD, 60 * NAD, NAD * 1000 + 1] {
            let (mut market,mut feed,mut fixture)=normalized();
            market.base_side.asset_decimals=base_decimals;
            market.quote_side.asset_decimals=quote_decimals;
            feed.price_nad=price_nad; feed.confidence_nad=0;
            fixture.data=feed.try_to_vec().unwrap();
            assert!(market.preview_add_liquidity(1_000_000_000,2_000_000_000).is_err());
            market.bind_oracle(&[fixture.info()],&clock()).unwrap();
            let r=market.preview_add_liquidity(1_000_000_000,2_000_000_000).unwrap();
            assert!(r.base_reserve_credit<=1_000_000_000 && r.quote_reserve_credit<=2_000_000_000);
            let base=market.normalize_amount(r.base_reserve_credit.into(),base_decimals).unwrap();
            let quote=market.normalize_amount(r.quote_reserve_credit.into(),quote_decimals).unwrap();
            let quote_atom=market.normalize_amount(1,quote_decimals).unwrap();
            // Actual reserve ratio differs from the source by less than one
            // quote atom, including unlike mint precision and fractional prices.
            assert!(quote*u128::from(NAD)>=base*u128::from(price_nad));
            assert!((quote-quote_atom)*u128::from(NAD)<base*u128::from(price_nad));
            let again=market.preview_add_liquidity(r.base_reserve_credit,r.quote_reserve_credit).unwrap();
            assert_eq!((again.base_reserve_credit,again.quote_reserve_credit,again.ylp_amount),
                (r.base_reserve_credit,r.quote_reserve_credit,r.ylp_amount));
        }
    }
    let (mut market,_,mut fixture)=normalized();
    market.bind_oracle(&[fixture.info()],&clock()).unwrap();
    assert!(market.preview_add_liquidity(1,1).is_err());
    market.oracle.policy.lambda_bps=0;
    market.bind_oracle(&[],&Clock::default()).unwrap();
    let r=market.preview_add_liquidity(1_000_000,2_000_000).unwrap();
    assert_eq!((r.base_reserve_credit,r.quote_reserve_credit),(1_000_000,2_000_000));
}

#[test]
fn normalized_reports_require_identity_source_age_and_confidence() {
    let (market,feed,mut fixture)=normalized();
    let policy=market.oracle.policy;
    policy.validate().unwrap();
    let read=|account:AccountInfo|policy.read(&[account],&clock(),market.base_side.asset_mint,market.quote_side.asset_mint);
    assert_eq!(read(fixture.info()).unwrap().price_nad,60*NAD);
    for case in 0..13 {
        let mut bad=feed;
        match case {
            0=>bad.magic[0]^=1,
            1=>bad.feed_id[0]^=1,
            2=>bad.base_mint=Pubkey::new_unique(),
            3=>bad.quote_mint=Pubkey::new_unique(),
            4=>bad.price_nad=0,
            5=>bad.confidence_nad=bad.price_nad,
            6=>bad.confidence_nad=bad.price_nad/100+1,
            7=>bad.publish_time=1001,
            8=>bad.publish_time=969,
            9=>bad.publish_slot=101,
            10=>bad.publish_slot=79,
            11=>bad.sequence=0,
            12=>bad.status=2,
            _=>unreachable!(),
        }
        fixture.data=bad.try_to_vec().unwrap();
        assert!(read(fixture.info()).is_err(),"accepted case {case}");
    }
    fixture.data=feed.try_to_vec().unwrap();fixture.owner=Pubkey::new_unique();
    assert!(read(fixture.info()).is_err());
    fixture.owner=policy.provider;fixture.key=Pubkey::new_unique();
    assert!(read(fixture.info()).is_err());
    fixture.key=policy.base_account;fixture.data.truncate(20);
    assert!(read(fixture.info()).is_err());
}

#[test]
fn replay_and_deserialization_cannot_reuse_execution_authority() {
    let (mut market,mut feed,mut fixture)=normalized();
    market.bind_oracle(&[fixture.info()],&clock()).unwrap();
    assert_eq!(market.oracle.runtime.observation().unwrap().price_nad,60*NAD);
    assert_eq!(market.curve_revision,1);
    market.bind_oracle(&[fixture.info()],&clock()).unwrap();
    assert_eq!(market.curve_revision,1);
    // A cached source observation survives serialization; its capability does not.
    let bytes=market.oracle.try_to_vec().unwrap();
    assert_eq!(bytes.len(),OracleState::INIT_SPACE);
    let decoded=OracleState::try_from_slice(&bytes).unwrap();
    assert_eq!(decoded.last_observation,market.oracle.last_observation);
    assert!(decoded.runtime.observation().is_err());
    let prior=market.oracle.last_observation;
    feed.price_nad+=NAD;fixture.data=feed.try_to_vec().unwrap();
    assert!(market.bind_oracle(&[fixture.info()],&clock()).is_err());
    assert_eq!(market.oracle.last_observation,prior);
    feed.sequence+=1;feed.publish_time-=1;fixture.data=feed.try_to_vec().unwrap();
    assert!(market.bind_oracle(&[fixture.info()],&clock()).is_err());
    feed.publish_time+=2;fixture.data=feed.try_to_vec().unwrap();
    market.bind_oracle(&[fixture.info()],&clock()).unwrap();
    assert_eq!(market.curve_revision,2);
}

#[test]
fn zero_weight_needs_no_feed_and_drops_prior_capability() {
    let (mut market,_,mut fixture)=normalized();
    market.bind_oracle(&[fixture.info()],&clock()).unwrap();
    market.oracle.policy.lambda_bps=0;
    market.bind_oracle(&[],&Clock::default()).unwrap();
    assert!(market.oracle.runtime.observation().is_err());
    assert!(market.bounded_curve().unwrap().is_none());
}

#[derive(AnchorSerialize)]
enum Verification {Partial {num_signatures:u8},Full}
#[derive(AnchorSerialize)]
struct PriceMessage {
    feed_id:[u8;32],price:i64,conf:u64,exponent:i32,publish_time:i64,prev_publish_time:i64,ema_price:i64,ema_conf:u64,
}
#[derive(AnchorSerialize)]
struct PriceAccount {write_authority:Pubkey,verification:Verification,message:PriceMessage,posted_slot:u64}
fn pyth(feed_id:[u8;32],price:i64,verification:Verification,time:i64,slot:u64)->Vec<u8> {
    let value=PriceAccount {write_authority:Pubkey::new_unique(),verification,message:PriceMessage{
        feed_id,price,conf:0,exponent:-8,publish_time:time,prev_publish_time:time-1,ema_price:price,ema_conf:0},posted_slot:slot};
    let mut data=hashv(&[b"account:PriceUpdateV2"]).to_bytes()[..8].to_vec();
    value.serialize(&mut data).unwrap();data.resize(134,0);data
}

#[test]
fn pyth_ratio_reads_both_assets_and_requires_full_verification() {
    let base_key=Pubkey::new_unique();let quote_key=Pubkey::new_unique();
    let policy=OraclePolicy {format:PYTH_RATIO_V2,provider:PYTH_RECEIVER,base_account:base_key,quote_account:quote_key,
        base_feed_id:[1;32],quote_feed_id:[2;32],lambda_bps:5000,adjustment_cap_nad:2*NAD,
        max_age_seconds:30,max_age_slots:20,max_confidence_bps:100,max_publish_skew_seconds:5};
    let mut base=FeedAccount {key:base_key,owner:PYTH_RECEIVER,lamports:1,data:pyth([1;32],2_000_000_000,Verification::Full,995,95)};
    let mut quote=FeedAccount {key:quote_key,owner:PYTH_RECEIVER,lamports:1,data:pyth([2;32],101_000_000,Verification::Full,996,96)};
    fn read<'a>(policy:OraclePolicy,a:AccountInfo<'a>,b:AccountInfo<'a>)->Result<OracleObservation> {policy.read(&[a,b],&clock(),Pubkey::default(),Pubkey::default())}
    let observed=read(policy,base.info(),quote.info()).unwrap();
    assert_eq!(observed.price_nad,19_801_980_198);
    assert_eq!(observed.source_times,[995,996]);
    assert_eq!(observed.source_slots,[95,96]);
    for signatures in [0,1,13,255] {
        base.data=pyth([1;32],2_000_000_000,Verification::Partial{num_signatures:signatures},995,95);
        assert!(read(policy,base.info(),quote.info()).is_err());
    }
    base.data=pyth([1;32],2_000_000_000,Verification::Full,995,95);
    for (time,slot) in [(969,95),(1001,95),(989,95),(995,79),(995,101)] {
        quote.data=pyth([2;32],101_000_000,Verification::Full,time,slot);
        assert!(read(policy,base.info(),quote.info()).is_err(),"accepted time={time} slot={slot}");
    }
    quote.data=pyth([2;32],101_000_000,Verification::Full,996,96);
    for price in [0,-1] {
        base.data=pyth([1;32],price,Verification::Full,995,95);
        assert!(read(policy,base.info(),quote.info()).is_err());
    }
}

#[test]
fn ratio_rounding_handles_different_exponents_without_a_quote_peg() {
    for exponent in -18_i32..=18 {
        let scale=10_u128.pow(exponent.unsigned_abs());
        for (a,b) in [(1_u64,3_u64),(100,101),(u32::MAX as u64,998_413)] {
            let (n,d)=if exponent>=0 {(u128::from(a)*u128::from(NAD)*scale,u128::from(b))}
                else {(u128::from(a)*u128::from(NAD),u128::from(b)*scale)};
            let low=n/d;let high=n.div_ceil(d);
            if high>u64::MAX as u128 {assert!(ratio_nad(a,b,exponent,true).is_err());}
            else {assert_eq!(ratio_nad(a,b,exponent,false).unwrap() as u128,low);assert_eq!(ratio_nad(a,b,exponent,true).unwrap() as u128,high);}
        }
    }
}

#[test]
fn pyth_direct_prices_the_selected_pair_with_one_verified_account() {
    let (_, _, mut fixture)=normalized();
    fixture.owner=PYTH_RECEIVER;
    fixture.data=pyth([7;32],123_456_789,Verification::Full,995,95);
    let policy=OraclePolicy {format:PYTH_DIRECT_V3,provider:PYTH_RECEIVER,base_account:fixture.key,
        base_feed_id:[7;32],lambda_bps:3500,adjustment_cap_nad:2*NAD,max_age_seconds:30,max_age_slots:20,
        max_confidence_bps:100,..Default::default()};
    policy.validate().unwrap();
    let observed=policy.read(&[fixture.info()],&clock(),Pubkey::new_unique(),Pubkey::new_unique()).unwrap();
    assert_eq!(observed.price_nad,1_234_567_890);
    assert_eq!(observed.confidence_nad,0);
    assert_eq!(observed.source_times,[995,0]);
    for offset in [40,41] {let saved=fixture.data[offset];fixture.data[offset]^=1;
        assert!(policy.read(&[fixture.info()],&clock(),Pubkey::default(),Pubkey::default()).is_err());
        fixture.data[offset]=saved;}
    for bad in [OraclePolicy {quote_account:Pubkey::new_unique(),..policy},
        OraclePolicy {max_publish_skew_seconds:1,..policy},OraclePolicy {provider:Pubkey::new_unique(),..policy}] {
        assert!(bad.validate().is_err());
    }
}

#[test]
fn normalized_ratio_authenticates_both_assets_denomination_and_uncertainty() {
    let (mut market,mut base,mut a)=normalized();
    let denomination=Pubkey::new_unique();
    base.quote_mint=denomination;
    base.price_nad=20*NAD;base.confidence_nad=NAD/100;
    let mut quote=NormalizedOracleV1 {base_mint:market.quote_side.asset_mint,feed_id:[8;32],
        price_nad:NAD,confidence_nad:NAD/1000,..base};
    let mut b=FeedAccount {key:Pubkey::new_unique(),owner:a.owner,lamports:1,data:quote.try_to_vec().unwrap()};
    let policy=OraclePolicy {format:NORMALIZED_RATIO_V4,quote_account:b.key,quote_feed_id:quote.feed_id,
        max_publish_skew_seconds:5,..market.oracle.policy};
    policy.validate().unwrap();market.oracle.policy=policy;a.data=base.try_to_vec().unwrap();
    let observation=policy.read(&[a.info(),b.info()],&clock(),market.base_side.asset_mint,market.quote_side.asset_mint).unwrap();
    assert_eq!(observation.price_nad,20*NAD);
    assert!(observation.confidence_nad>=30_000_000 && observation.confidence_nad<=30_031_000);
    market.bind_oracle(&[a.info(),b.info()],&clock()).unwrap();
    // Independent updates do not require synchronized counters, and the idle
    // leg must still be immutable at its previously observed counter.
    base.sequence+=1;base.publish_time+=1;base.publish_slot+=1;base.price_nad+=NAD;
    a.data=base.try_to_vec().unwrap();
    market.bind_oracle(&[a.info(),b.info()],&clock()).unwrap();
    let saved=market.oracle.last_observation;
    quote.price_nad+=1;b.data=quote.try_to_vec().unwrap();
    assert!(market.bind_oracle(&[a.info(),b.info()],&clock()).is_err());
    assert_eq!(market.oracle.last_observation,saved);
    quote.sequence+=1;quote.publish_time+=1;quote.publish_slot+=1;
    b.data=quote.try_to_vec().unwrap();market.bind_oracle(&[a.info(),b.info()],&clock()).unwrap();
    // Advancing the minimum aggregate sequence must not hide one leg's rollback.
    base.sequence=1;base.publish_time+=1;base.publish_slot+=1;a.data=base.try_to_vec().unwrap();
    quote.sequence=10;b.data=quote.try_to_vec().unwrap();
    assert!(market.bind_oracle(&[a.info(),b.info()],&clock()).is_err());
    base.sequence=3;a.data=base.try_to_vec().unwrap();
    for case in 0..13 {
        let mut bad=quote;
        match case {
            0=>bad.quote_mint=Pubkey::new_unique(),1=>bad.base_mint=market.base_side.asset_mint,
            2=>bad.feed_id=[9;32],3=>bad.price_nad=0,4=>bad.confidence_nad=bad.price_nad,
            5=>bad.confidence_nad=bad.price_nad/2,6=>bad.status=0,7=>bad.sequence=0,
            8=>bad.publish_time=1001,9=>bad.publish_time=969,10=>bad.publish_slot=101,
            11=>bad.publish_slot=79,12=>bad.publish_time=990,_=>unreachable!(),
        }
        b.data=bad.try_to_vec().unwrap();
        assert!(policy.read(&[a.info(),b.info()],&clock(),market.base_side.asset_mint,market.quote_side.asset_mint).is_err(),"accepted {case}");
    }
    b.data=quote.try_to_vec().unwrap();b.owner=Pubkey::new_unique();
    assert!(policy.read(&[a.info(),b.info()],&clock(),market.base_side.asset_mint,market.quote_side.asset_mint).is_err());
    b.owner=a.owner;
    assert!(policy.read(&[a.info()],&clock(),market.base_side.asset_mint,market.quote_side.asset_mint).is_err());
    base.quote_mint=Pubkey::default();quote.quote_mint=Pubkey::default();
    a.data=base.try_to_vec().unwrap();b.data=quote.try_to_vec().unwrap();
    assert!(policy.read(&[a.info(),b.info()],&clock(),market.base_side.asset_mint,market.quote_side.asset_mint).is_err());
}
