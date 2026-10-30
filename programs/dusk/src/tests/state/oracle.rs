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
