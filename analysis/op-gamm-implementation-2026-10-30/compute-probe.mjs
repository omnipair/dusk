import {LiteSVM,ComputeBudget,FeatureSet} from 'litesvm';
import {PublicKey,Keypair,Transaction,TransactionInstruction,ComputeBudgetProgram} from '@solana/web3.js';
import {writeFileSync,readFileSync} from 'node:fs';

const program=Keypair.generate().publicKey;
const payer=Keypair.generate();
const budget=new ComputeBudget();
const profiling=process.env.OP_GAMM_DIAGNOSTIC_UNLIMITED==='1';
if(profiling) budget.computeUnitLimit=100_000_000n;
const svm=new LiteSVM();
const featureSet=new FeatureSet();
const featureOwner=new PublicKey('Feature111111111111111111111111111111111111');
for (const id of featureSet.getInactiveFeatures()) {
 const account=svm.getAccount(new PublicKey(id));
 if(account?.owner.equals(featureOwner)&&account.data.length===9&&account.data[0]===1)
  featureSet.activate(id,Buffer.from(account.data).readBigUInt64LE(1));
}
featureSet.activate(new PublicKey('Eoh7e1sDqtyPtuiWAhBNSJinvtJWTTDgeUMRi3RF8zWS').toBytes(),0n);
svm.withFeatureSet(featureSet).withComputeBudget(budget).withBuiltins().withDefaultPrograms().withPrecompiles();
svm.addProgramFromFile(program,'target/deploy/op_gamm_compute.so');
svm.airdrop(payer.publicKey,1_000_000_000n);
const rows=[];
const extended=process.env.OP_GAMM_EXTENDED==='1';
const solver=process.env.OP_GAMM_SOLVER==='1';
const cases=[];
if(extended) {
 for(const line of readFileSync('analysis/op-gamm-implementation-2026-10-30/invariant-reference.csv','utf8').trim().split(/\r?\n/).slice(1)) {
  const n=line.split(',').map(BigInt);
  cases.push({amp:Number(n[0]),core:Number(n[1]),fade:Number(n[2]),lambda:Number(n[3]),
   reference:n[4]*1_000_000_000n,cap:n[5],start:[n[6],n[7]],end:[n[8],n[9]],input_bps:null});
 }
} else {
 for(const amp of [1,4]) for(const input_bps of [1,10,100,1000]) {
  const start=[10_000_000_000_000n,600_000_000_000_000n];
  const end=[start[0]+start[0]*BigInt(input_bps)/10_000n,start[1]-start[1]*BigInt(input_bps)/10_000n];
  cases.push({amp,core:amp>1?1000:0,fade:amp>1?4000:0,lambda:5000,
   reference:60_000_000_000n,cap:2_000_000_000n,start,end,input_bps});
 }
}
for(const [caseIndex,c] of cases.entries()) {
 if(process.env.OP_GAMM_CASE && caseIndex!==Number(process.env.OP_GAMM_CASE)) continue;
 const {amp,core,fade,lambda,reference,cap,start,end,input_bps}=c;
 const data=Buffer.alloc(solver?103:102);
 const forward=end[0]>=start[0];
 const effectiveEnd=solver?[forward?end[0]-start[0]:end[1]-start[1],1n]:end;
 if(solver) data[102]=forward?1:2;
 data.writeBigUInt64LE(BigInt(amp));data.writeUInt16LE(lambda,8);data.writeBigUInt64LE(reference,10);
 data.writeBigUInt64LE(60_000_000_000n,18);data.writeBigUInt64LE(cap,26);
 data.writeUInt16LE(core,34);data.writeUInt16LE(fade,36);
 [...start,...effectiveEnd].forEach((n,i)=>{data.writeBigUInt64LE(n&((1n<<64n)-1n),38+i*16);data.writeBigUInt64LE(n>>64n,46+i*16);});
 const tx=new Transaction().add(...(profiling?[]:[ComputeBudgetProgram.setComputeUnitLimit({units:1_400_000})]),
  ComputeBudgetProgram.requestHeapFrame({bytes:256*1024}),new TransactionInstruction({programId:program,keys:[],data}));
 tx.feePayer=payer.publicKey;tx.recentBlockhash=svm.latestBlockhash();tx.sign(payer);
 const result=svm.simulateTransaction(tx);
 const meta=result.meta?.()??result;
 const row={solver,diagnostic_unlimited:profiling,amp,core,fade,lambda,reference:reference.toString(),input_bps,start:start.map(String),end:end.map(String),error:result.err?.()?.toString()??null,compute:Number(meta.computeUnitsConsumed?.()),logs:meta.logs?.()};
 rows.push(row);console.log(JSON.stringify(row));
}
writeFileSync('analysis/op-gamm-implementation-2026-10-30/'+(solver?'solver-':'')+(extended?'extended-':'')+(process.env.OP_GAMM_CASE?'case-'+process.env.OP_GAMM_CASE+'-':'')+(profiling?'compute-profile-unlimited.json':'compute-probe.json'),JSON.stringify(rows,null,2)+'\n');
