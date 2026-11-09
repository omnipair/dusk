// Deterministic TEST-DATA conversion. Original devnet captures stay untouched.
// The pinned predecessor is the final layout-13 revision.
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
  market.version=14;
  market.config.irm.lower_utilization_bps=7_500;
  market.config.irm.upper_utilization_bps=old.config.irm.borrow_utilization_limit_bps;
  const body=Buffer.alloc(32_768);
  const size=newCoder.accountLayouts.get('Market').layout.encode(market,body);
  return Buffer.concat([newCoder.accountDiscriminator('Market'),body.subarray(0,size)]);
}
for(const name of ['hlp-entry','vob-market','hlp-entry-funding']) {
  const target=`programs/dusk/src/tests/fixtures/${name}-layout14.bin`;
  const data=encode(original(`programs/dusk/src/tests/fixtures/${name}-layout13.bin`));
  writeFileSync(target,data);
  process.stdout.write(`${target} ${createHash('sha256').update(data).digest('hex')}\n`);
}
const previewPath='tests/fixtures/market-preview/market.bin';
const previewData=encode(original(previewPath));
const previewHash=createHash('sha256').update(previewData).digest('hex');
writeFileSync(previewPath,previewData);
const previewSnapshot=JSON.parse(original('tests/fixtures/market-preview/snapshot.json'));
previewSnapshot.sha256=previewHash;
previewSnapshot.schemaAdaptation='Layout 14: independent utilization and risk-coverage target bands.';
writeFileSync('tests/fixtures/market-preview/snapshot.json',JSON.stringify(previewSnapshot,null,2)+'\n');
process.stdout.write(`${previewPath} ${previewHash}\n`);
const batch=JSON.parse(original('packages/dusk-sdk/scripts/fixtures/virtual-book-batch-layout13.json'));
batch.result.value.accounts[0].data[0]=encode(Buffer.from(batch.result.value.accounts[0].data[0],'base64')).toString('base64');
batch.result.value.accounts[0].space=Buffer.from(batch.result.value.accounts[0].data[0],'base64').length;
const oldTypes=new BorshCoder(oldIdl).types,newTypes=new BorshCoder(newIdl).types;
function preview(data) {
  const decoded=oldTypes.decode('SwapPreview',Buffer.from(data,'base64'));
  return newTypes.encode('SwapPreview',adapt(decoded,{defined:{name:'SwapPreview'}})).toString('base64');
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
batch.schemaAdaptation='Layout 14: independent utilization and risk-coverage target bands. Existing financial state is preserved; the former utilization admission threshold becomes the utilization rate upper target.';
writeFileSync('packages/dusk-sdk/scripts/fixtures/virtual-book-batch-layout14.json',JSON.stringify(batch,null,2)+'\n');
