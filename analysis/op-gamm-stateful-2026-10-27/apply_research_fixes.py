"""Apply switchable candidate fixes ONLY to the archived research source."""
from pathlib import Path
import subprocess
R=Path(__file__).resolve().parent;S=R/'engine-snapshot/programs/dusk/src';PIN='77877d576018fd2bd8191d13f05534b0484e2282'
def source(rel):return subprocess.check_output(['git','show',PIN+':programs/dusk/src/'+rel],text=True)
f='transitions/liquidity/hlp/integrated.rs';s=source(f)
start=s.index('pub(crate) fn reconstruct_hlp_endpoint(');end=s.index('\n/// Applies an hLP-funded',start)
part=s[start:end]
part=part.replace('let (base, quote) = state.book_ordinary()?;', '''let (mut base, mut quote) = state.book_ordinary()?;
    if crate::benchmark_api::RESEARCH_FIX_FROZEN.load(std::sync::atomic::Ordering::Relaxed) {
        if state.frozen_hlp_mask & 2 != 0 { base=base.checked_add(state.quote_hlp_base_debt).ok_or(ErrorCode::MarketMathOverflow)?; }
        if state.frozen_hlp_mask & 1 != 0 { quote=quote.checked_add(state.base_hlp_quote_debt).ok_or(ErrorCode::MarketMathOverflow)?; }
    }
    if crate::benchmark_api::RESEARCH_FIX_CARRY.load(std::sync::atomic::Ordering::Relaxed) {
        base=base.checked_add(u128::from(state.quote_hlp_base_carry)*state.base_atom_nad).ok_or(ErrorCode::MarketMathOverflow)?;
        quote=quote.checked_add(u128::from(state.base_hlp_quote_carry)*state.quote_atom_nad).ok_or(ErrorCode::MarketMathOverflow)?;
    }''')
s=s[:start]+part+s[end:]
needle='        let base = signed(endpoint.total_base)?'
idx=s.index(needle)
s=s[:idx]+'''        // A frozen vault is already removed by the persistent frozen offsets;
        // valuing its retained debt again as a zero-equity active hedge double counts it.
        let fixed=crate::benchmark_api::RESEARCH_FIX_FROZEN.load(std::sync::atomic::Ordering::Relaxed);
        let base_equity=if fixed && self.frozen_hlp_mask & 1 != 0 {0}else{base_equity};
        let quote_equity=if fixed && self.frozen_hlp_mask & 2 != 0 {0}else{quote_equity};
'''+s[idx:]
(S/f).write_text(s)
f='transitions/liquidity/hlp/engine.rs';s=source(f)
needle='    let mut ownership = reconstruct_hlp_ownership(\n        static_supply,\n        book_base,\n        book_quote,'
assert needle in s
s=s.replace(needle,'''    let fixed=crate::benchmark_api::RESEARCH_FIX_FROZEN.load(std::sync::atomic::Ordering::Relaxed);
    let carry=crate::benchmark_api::RESEARCH_FIX_CARRY.load(std::sync::atomic::Ordering::Relaxed);
    let ownership_base=book_base
        .checked_add(if fixed && frozen_mask & 2 != 0 {end.quote_hlp_base_debt}else{0})
        .and_then(|v|v.checked_add(if carry {u128::from(end.quote_hlp_base_carry)*end.base_atom_nad}else{0}))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let ownership_quote=book_quote
        .checked_add(if fixed && frozen_mask & 1 != 0 {end.base_hlp_quote_debt}else{0})
        .and_then(|v|v.checked_add(if carry {u128::from(end.base_hlp_quote_carry)*end.quote_atom_nad}else{0}))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let mut ownership = reconstruct_hlp_ownership(
        static_supply,
        ownership_base,
        ownership_quote,''')
(S/f).write_text(s)
s=source('benchmark_api.rs')+'\n'+(R/'native_adapter.rs').read_text();(S/'benchmark_api.rs').write_text(s)
p=R/'native/src/main.rs';s=p.read_text();needle='   if op=="repay"'
if 'if op=="fixes"' not in s:s=s.replace(needle,'''   if op=="fixes"{research_fixes(q["frozen"].as_bool().unwrap_or(false),q["carry"].as_bool().unwrap_or(false));return Ok(json!({"ok":true}));}
'''+needle)
p.write_text(s)
