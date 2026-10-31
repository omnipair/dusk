/** Devnet mock publisher. DUSK_RPC_URL and MOCK_ORACLE_KEYPAIR are required.
 * node scripts/devnet/mock_oracle.mjs publish 1.00
 * node scripts/devnet/mock_oracle.mjs watch
 * node scripts/devnet/mock_oracle.mjs invalidate
 * This is a synthetic, centralized test feed, never external market data. */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, TransactionInstruction, sendAndConfirmTransaction } from '@solana/web3.js';
const manifest=JSON.parse(readFileSync(fileURLToPath(new URL('./oracles/meta-usdc.json',import.meta.url)),'utf8'));
const rpc=process.env.DUSK_RPC_URL;
if(!rpc || !process.env.MOCK_ORACLE_KEYPAIR)throw new Error('Explicit devnet RPC and mock publisher keypair required');
const connection=new Connection(rpc,'confirmed');
if(await connection.getGenesisHash()!=='EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG')throw new Error('Refuse non-devnet');
const signer=Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(process.env.MOCK_ORACLE_KEYPAIR,'utf8'))));
if(signer.publicKey.toBase58()!==manifest.publisher)throw new Error('Wrong mock publisher');
const program=new PublicKey(manifest.programId),report=new PublicKey(manifest.report);
const exact=(value)=>{if(!/^(0|[1-9]\d*)(\.\d{1,9})?$/.test(value))throw new Error('Positive decimal with at most 9 places required');const[a,b='']=value.split('.');return BigInt(a)*1000000000n+BigInt(b.padEnd(9,'0'));};
async function publish(action,priceText){
 const current=await connection.getAccountInfo(report,'confirmed');
 if(current && (!current.owner.equals(program)||current.data.length!==145))throw new Error('Invalid mock account');
 const price=priceText ? exact(priceText) : current?.data.readBigUInt64LE(104)??1000000000n;
 const confidence=current?.data.readBigUInt64LE(112)??1000000n;
 if(price<=confidence)throw new Error('Price must exceed mock confidence width');
 const status=action==='invalidate'?0:action==='publish'?1:current?.data[144]??1;
 const data=Buffer.alloc(121);Buffer.from(manifest.feedId,'hex').copy(data,0);new PublicKey(manifest.baseMint).toBuffer().copy(data,32);new PublicKey(manifest.quoteMint).toBuffer().copy(data,64);data.writeBigUInt64LE(price,96);data.writeBigUInt64LE(confidence,104);data[112]=status;data.writeBigUInt64LE(current?.data.readBigUInt64LE(136)??0n,113);
 const tx=new Transaction().add(new TransactionInstruction({programId:program,keys:[{pubkey:signer.publicKey,isSigner:true,isWritable:true},{pubkey:report,isSigner:false,isWritable:true},{pubkey:SystemProgram.programId,isSigner:false,isWritable:false}],data}));
 const signature=await sendAndConfirmTransaction(connection,tx,[signer],{commitment:'confirmed'});
 console.log(JSON.stringify({signature,report:manifest.report,priceNad:price.toString(),status}));
}
const action=process.argv[2]??'publish';if(!['publish','watch','invalidate'].includes(action))throw new Error('Use publish, watch, or invalidate');
try{do{await publish(action,process.argv[3]);if(action!=='watch')break;await new Promise(r=>setTimeout(r,15000));}while(true);}catch(error){console.error(String(error?.message??error).replaceAll(rpc,'[devnet RPC]'));process.exitCode=1;}
// web3.js keeps its confirmation WebSocket alive after a one-shot command.
if(action !== 'watch') process.exit(process.exitCode ?? 0);
