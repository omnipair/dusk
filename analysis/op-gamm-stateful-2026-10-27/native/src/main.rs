use std::io::{self,BufRead,Write};
use serde_json::{json,Value};
use dusk::{benchmark_api::*,state::{Market,Debt}};
fn number(v:&Value)->u128 {v.as_str().map(|s|s.parse().unwrap()).unwrap_or(v.as_u64().unwrap_or(0) as u128)}
fn array(q:&Value)->[i128;17]{let mut v=[0i128;17];for(i,a)in q["s"].as_array().unwrap().iter().enumerate(){v[i]=a.as_str().unwrap().parse().unwrap();}v}
fn stats(m:&Market,mark:u128,carry:bool)->Result<Value,String>{
 let mut v=json!({"meta":research_meta(m).map_err(|e|e.to_string())?,
 "point":research_point(m).map_err(|e|e.to_string())?.map(|x|x.to_string()),
 "books":research_books(m).map_err(|e|e.to_string())?.map(|x|x.to_string())});
 match research_read(m,mark,carry){Ok(s)=>v["s"]=json!(s.map(|x|x.to_string())),Err(e)=>v["state_error"]=json!(e.to_string())};Ok(v)
}
fn main(){
 let mut markets:Vec<Market>=Vec::new();
 for line in io::stdin().lock().lines(){
  let result=(||->Result<Value,String>{
   let q:Value=serde_json::from_str(&line.map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
   let amp=q["amp"].as_u64().unwrap_or(1);let mark=number(&q["mark"]);let carry=q["carry"].as_bool().unwrap_or(false);
   let op=q["op"].as_str().unwrap_or("kernel");
   if op=="fixes"{research_fixes(q["frozen"].as_bool().unwrap_or(false),q["carry"].as_bool().unwrap_or(false));return Ok(json!({"ok":true}));}
   if op=="repay"{let v=Debt::repayment_for_max(number(&q["position"]),number(&q["aggregate"]),number(&q["index"]),number(&q["maximum"]) as u64).map_err(|e|e.to_string())?;return Ok(json!({"burn":v.shares_to_burn.to_string(),"cash":v.cash_repaid,"reduced":v.position_debt_reduced,"remaining":v.remaining_position_debt}));}
   if op=="fixture" {let(s,m)=research_fixture([q["db"].as_u64().unwrap_or(9) as u8,q["dq"].as_u64().unwrap_or(6) as u8],q["kind"].as_u64().unwrap_or(0) as u8,amp).map_err(|e|e.to_string())?;return Ok(json!({"s":s.map(|x|x.to_string()),"meta":m}));}
   if op=="kernel"{let(s,p,e,out)=research_kernel(array(&q),number(&q["amount"]),q["direction"].as_u64().unwrap_or(0) as u8,amp,60_000_000_000).map_err(|e|e.to_string())?;return Ok(json!({"s":s.map(|v|v.to_string()),"point":p.map(|v|v.to_string()),"endpoint":e.map(|v|v.to_string()),"out":out.to_string()}));}
   let id=q["id"].as_u64().unwrap_or(0) as usize;
   if op=="stats" {return stats(&markets[id],mark,carry);}
   let mut recovery=None;let mut native_out=None;
   let n=match op {
    "new"=>research_make([q["db"].as_u64().unwrap_or(9) as u8,q["dq"].as_u64().unwrap_or(6) as u8],q["kind"].as_u64().unwrap_or(0) as u8,amp),
    "settle"=>research_settle(&markets[id],array(&q),q["direction"].as_u64().unwrap_or(0) as u8,number(&q["input"]) as u64,number(&q["out"]) as u64),
    "native"=>research_native(&markets[id],q["direction"].as_u64().unwrap_or(0) as u8,number(&q["input"]) as u64).map(|(m,o)|{native_out=Some(o);m}),
    "advance"=>research_advance(&markets[id],number(&q["slot"]) as u64),
    "recover"=>research_recover(&markets[id],q["side"].as_u64().unwrap_or(0) as u8).map(|(m,r)|{recovery=Some(r);m}),
    _=>return Err("unknown operation".into())
   }.map_err(|e|e.to_string())?;
   let mut v=stats(&n,mark,carry)?;
   if let Some(o)=native_out{v["out"]=json!(o.to_string());}
   if let Some(r)=recovery{v["recovery"]=json!(r);}
   if q["commit"].as_bool().unwrap_or(true){v["id"]=json!(markets.len());markets.push(n);}
   Ok(v)
  })();
  println!("{}",match result{Ok(v)=>v,Err(e)=>json!({"error":e})});io::stdout().flush().unwrap();
 }
}
