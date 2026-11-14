// Deterministic TEST-DATA conversion. Original devnet captures stay untouched.
// The pinned predecessor is the final layout-13 revision. Fixtures are derived
// at the current market layout, the only one the program reads.
const {execFileSync} = require('node:child_process');
const {readFileSync,writeFileSync} = require('node:fs');
const {createHash} = require('node:crypto');
const {BorshAccountsCoder,BorshCoder,BN} = require('@coral-xyz/anchor');
const {PublicKey} = require('@solana/web3.js');
const predecessor='06094229d4a5a3858bf16bb4a2119157ca67b1cb';
const original=path=>execFileSync('git',['show',`${predecessor}:${path}`],{maxBuffer:16*1024*1024});
const oldIdl=JSON.parse(original('packages/dusk-sdk/src/idl_v2.json'));
const newIdl=JSON.parse(readFileSync('target/idl/dusk.json'));
const oldCoder=new BorshAccountsCoder(oldIdl),newCoder=new BorshAccountsCoder(newIdl);
const zeroKey=new PublicKey(new Uint8Array(32));
const constant=name=>Number(newIdl.constants.find(entry=>entry.name===name).value);
const layout=constant('MARKET_LAYOUT_VERSION');
const minimumAmountDecimals=constant('MIN_AMOUNT_DECIMALS');
// Internal quantities the captures stored at max(9, base, quote) decimals.
// The current layout stores them at max(MIN_AMOUNT_DECIMALS, base, quote).
// Raw atoms, shares, prices, rates and per-share ratios keep their units.
const MARKET_AMOUNTS=[
  ['oracle','recognized_tail_liquidity'],
  ['amm','concentrated_curve_cache','tail_liquidity'],
  ['amm','concentrated_curve_cache','concentrated_liquidity'],
  ['amm','retention_required_nad'],['amm','retention_stop_nad'],['amm','retention_hard_cap_nad'],
  ['amm','deferred_controller_target','required_nad'],
  ['amm','deferred_controller_target','evaluated_base_reserve_nad'],
  ['amm','deferred_controller_target','evaluated_quote_reserve_nad'],
  ['amm','frozen_base_vault_base_offset_nad'],['amm','frozen_base_vault_quote_offset_nad'],
  ['amm','frozen_quote_vault_base_offset_nad'],['amm','frozen_quote_vault_quote_offset_nad'],
  ['base_hlp_vault','residual_exposure'],['quote_hlp_vault','residual_exposure'],
  ['base_hlp_vault','last_nav_nad'],['quote_hlp_vault','last_nav_nad'],
  ['risk','observed_curve_depth_nad'],['risk','curve_depth_ema_nad'],
];
const PREVIEW_AMOUNTS=[
  ['retention_required_nad'],['retention_stop_nad'],['retention_hard_cap_nad'],
  ['ordinary_base_reserve_nad'],['ordinary_quote_reserve_nad'],
];
function amountFactor(market) {
  const decimals=Math.max(market.base_side.asset_decimals,market.quote_side.asset_decimals);
  return new BN(10).pow(new BN(Math.max(minimumAmountDecimals,decimals)-Math.max(9,decimals)));
}
function restate(value,paths,factor) {
  for(const path of paths) {
    const parent=path.slice(0,-1).reduce((object,key)=>object[key],value);
    parent[path.at(-1)]=parent[path.at(-1)].mul(factor);
  }
}
function adapt(value,type) {
  if (typeof type==='string') {
    if (value!==undefined) return value;
    if(type==='pubkey')return zeroKey;
    if(type==='bool')return false;
    if(type==='string')return '';
    if(type==='bytes')return Buffer.alloc(0);
    return /^(u64|u128|i64|i128)$/.test(type)?new BN(0):0;
  }
  if(type.array)return Array.from({length:type.array[1]},(_,i)=>adapt(value?.[i],type.array[0]));
  if(type.vec)return (value??[]).map(v=>adapt(v,type.vec));
  if(type.option)return value==null?null:adapt(value,type.option);
  const def=newIdl.types.find(t=>t.name===(type.defined.name??type.defined));
  if(def.type.kind==='enum')return value??{[def.type.variants[0].name]:{}};
  return Object.fromEntries(def.type.fields.map(field=>[field.name,adapt(value?.[field.name] ?? (field.name==="revenue_escrow"?value?.protocol_escrow:field.name==="revenue_release_pending"?value?.protocol_release_pending:undefined),field.type)]));
}
function encode(data) {
  const old=oldCoder.decode('Market',data);
  const market=adapt(old,{defined:{name:'Market'}});
  market.version=layout;
  market.config.irm.lower_utilization_bps=7_500;
  market.config.irm.upper_utilization_bps=old.config.irm.borrow_utilization_limit_bps;
  restate(market,MARKET_AMOUNTS,amountFactor(market));
  return encodeAccount('Market',market);
}
function encodeAccount(name,value) {
  const body=Buffer.alloc(32_768);
  const size=newCoder.accountLayouts.get(name).layout.encode(value,body);
  return Buffer.concat([newCoder.accountDiscriminator(name),body.subarray(0,size)]);
}
const oldAuthority=oldCoder.decode('FutarchyAuthority',original('programs/dusk/src/tests/fixtures/vob-authority-20260929.bin'));
const authority=encodeAccount('FutarchyAuthority',adapt(oldAuthority,{defined:{name:'FutarchyAuthority'}}));
const authorityTarget=`programs/dusk/src/tests/fixtures/vob-authority-layout${layout}.bin`;
writeFileSync(authorityTarget,authority);
process.stdout.write(`${authorityTarget} ${createHash('sha256').update(authority).digest('hex')}\n`);
for(const name of ['hlp-entry','vob-market','hlp-entry-funding']) {
  const target=`programs/dusk/src/tests/fixtures/${name}-layout${layout}.bin`;
  const data=encode(original(`programs/dusk/src/tests/fixtures/${name}-layout13.bin`));
  writeFileSync(target,data);
  process.stdout.write(`${target} ${createHash('sha256').update(data).digest('hex')}\n`);
}
const batch=JSON.parse(original('packages/dusk-sdk/scripts/fixtures/virtual-book-batch-layout13.json'));
const batchFactor=amountFactor(oldCoder.decode('Market',Buffer.from(batch.result.value.accounts[0].data[0],'base64')));
batch.result.value.accounts[0].data[0]=encode(Buffer.from(batch.result.value.accounts[0].data[0],'base64')).toString('base64');
batch.result.value.accounts[0].space=Buffer.from(batch.result.value.accounts[0].data[0],'base64').length;
const oldTypes=new BorshCoder(oldIdl).types,newTypes=new BorshCoder(newIdl).types;
function preview(data) {
  const decoded=adapt(oldTypes.decode('SwapPreview',Buffer.from(data,'base64')),{defined:{name:'SwapPreview'}});
  restate(decoded,PREVIEW_AMOUNTS,batchFactor);
  return newTypes.encode('SwapPreview',decoded).toString('base64');
}
const prefix=`Program return: ${batch.programId} `;
const nextProgram=newIdl.address;
batch.result.value.logs=batch.result.value.logs.map(line=>line.startsWith(prefix)?`Program return: ${nextProgram} `+preview(line.slice(prefix.length)):line.replaceAll(batch.programId,nextProgram));
batch.result.value.returnData.data[0]=preview(batch.result.value.returnData.data[0]);
for (const account of batch.result.value.accounts) {
  if (account.owner===batch.programId) account.owner=nextProgram;
}
const adaptedMarket=newCoder.decode('Market',Buffer.from(batch.result.value.accounts[0].data[0],'base64'));
batch.market=PublicKey.findProgramAddressSync([Buffer.from('market_v2'),adaptedMarket.base_side.asset_mint.toBuffer(),
  adaptedMarket.quote_side.asset_mint.toBuffer(),Buffer.from(adaptedMarket.params_hash)],new PublicKey(nextProgram))[0].toBase58();
batch.programId=nextProgram;
batch.result.value.returnData.programId=nextProgram;
batch.derivedFrom=`layout-13 regression fixture at ${predecessor}`;
batch.schemaAdaptation=`Layout ${layout}: independent unconfigured continuous fee-auction curves, independent utilization and risk-coverage target bands, and amounts counted in at least ${minimumAmountDecimals} decimals. The two persistent hLP funding pricing offsets start at zero. Existing financial state is preserved: the former utilization admission threshold becomes the utilization rate upper target, and every stored curve, depth, retention, NAV and hLP offset quantity is restated at the twelve-decimal minimum.`;
const batchTarget=`packages/dusk-sdk/scripts/fixtures/virtual-book-batch-layout${layout}.json`;
writeFileSync(batchTarget,JSON.stringify(batch,null,2)+'\n');
process.stdout.write(`${batchTarget}\n`);
