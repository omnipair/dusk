/** Independent signed SBF regressions for the devnet fixture, not a protocol change. */
import assert from 'node:assert/strict';import{readFileSync}from'node:fs';
import{LiteSVM,FailedTransactionMetadata}from'litesvm';
import{Keypair,PublicKey,SystemProgram,Transaction,TransactionInstruction}from'@solana/web3.js';
const svm=new LiteSVM(),program=Keypair.generate().publicKey,alice=Keypair.generate(),bob=Keypair.generate();
svm.addProgram(program,new Uint8Array(readFileSync('target/deploy/devnet_oracle.so')));
svm.airdrop(alice.publicKey,1000000000n);svm.airdrop(bob.publicKey,1000000000n);
const feed=Buffer.alloc(32,1),base=Keypair.generate().publicKey,quote=Keypair.generate().publicKey;
const report=PublicKey.findProgramAddressSync([Buffer.from('mock-feed'),alice.publicKey.toBuffer(),feed],program)[0];
function send(ix,signer=alice){svm.expireBlockhash();const tx=new Transaction({feePayer:signer.publicKey,recentBlockhash:svm.latestBlockhash()}).add(ix);tx.sign(signer);return svm.sendTransaction(tx);}
function ix(signer=alice,overrides={}){const d=Buffer.alloc(121);feed.copy(d);base.toBuffer().copy(d,32);quote.toBuffer().copy(d,64);d.writeBigUInt64LE(overrides.price??1000000000n,96);d.writeBigUInt64LE(1000000n,104);d[112]=overrides.status??1;d.writeBigUInt64LE((svm.getAccount(report)?.data.length??0)>=145?Buffer.from(svm.getAccount(report).data).readBigUInt64LE(136):0n,113);if(overrides.pair)d[32]^=1;return new TransactionInstruction({programId:program,keys:[{pubkey:signer.publicKey,isSigner:true,isWritable:true},{pubkey:report,isSigner:false,isWritable:true},{pubkey:SystemProgram.programId,isSigner:false,isWritable:false}],data:d});}
// A hostile prefunding transfer cannot prevent the legitimate owner from allocating its PDA.
assert(!(send(SystemProgram.transfer({fromPubkey:alice.publicKey,toPubkey:report,lamports:1000000})) instanceof FailedTransactionMetadata));
assert(!(send(ix()) instanceof FailedTransactionMetadata));
let data=Buffer.from(svm.getAccount(report).data);assert.equal(data.length,145);assert.equal(data.subarray(0,8).toString(),'DOPRV001');assert.equal(data.readBigUInt64LE(136),1n);
assert(send(ix(bob),bob) instanceof FailedTransactionMetadata);assert(send(ix(alice,{pair:true})) instanceof FailedTransactionMetadata);assert(send(ix(alice,{price:0n})) instanceof FailedTransactionMetadata);
const staleRefresh=ix();
assert(!(send(ix(alice,{price:1200000000n})) instanceof FailedTransactionMetadata));data=Buffer.from(svm.getAccount(report).data);assert.equal(data.readBigUInt64LE(104),1200000000n);assert.equal(data.readBigUInt64LE(136),2n);assert(send(staleRefresh) instanceof FailedTransactionMetadata);
assert(!(send(ix(alice,{status:0})) instanceof FailedTransactionMetadata));assert.equal(Buffer.from(svm.getAccount(report).data)[144],0);
console.log('PASS: signed SBF initialize/prefunding, owner isolation, immutable pair, invalid price, monotone sequence and invalidation');
