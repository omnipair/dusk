use solana_program::{
    account_info::AccountInfo, entrypoint, entrypoint::ProgramResult, program::set_return_data,
    program_error::ProgramError, pubkey::Pubkey,
};
entrypoint!(process_instruction);
fn process_instruction(_: &Pubkey, _: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.len() != 102 && data.len() != 103 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let n = |i: usize| u128::from_le_bytes(data[i..i + 16].try_into().unwrap());
    let amp = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let lambda = u16::from_le_bytes(data[8..10].try_into().unwrap());
    let reference = u64::from_le_bytes(data[10..18].try_into().unwrap());
    if data.len() == 103 {
        let result = dusk::math::op_gamm::bounded_swap_probe(
            amp,
            u16::from_le_bytes(data[34..36].try_into().unwrap()),
            u16::from_le_bytes(data[36..38].try_into().unwrap()),
            u64::from_le_bytes(data[18..26].try_into().unwrap()),
            lambda,
            reference,
            u64::from_le_bytes(data[26..34].try_into().unwrap()),
            [n(38), n(54)],
            n(70),
            n(86),
            data[102] == 1,
        )?;
        let mut output = [0_u8; 48];
        for (i, value) in result.iter().enumerate() {
            output[i * 16..(i + 1) * 16].copy_from_slice(&value.to_le_bytes());
        }
        set_return_data(&output);
        return Ok(());
    }
    let (lo, hi) = dusk::math::op_gamm::bounded_invariant_probe(
        amp,
        u16::from_le_bytes(data[34..36].try_into().unwrap()),
        u16::from_le_bytes(data[36..38].try_into().unwrap()),
        u64::from_le_bytes(data[18..26].try_into().unwrap()),
        lambda,
        reference,
        u64::from_le_bytes(data[26..34].try_into().unwrap()),
        [n(38), n(54)],
        [n(70), n(86)],
    )?;
    let mut out = [0_u8; 32];
    out[..16].copy_from_slice(&lo.to_le_bytes());
    out[16..].copy_from_slice(&hi.to_le_bytes());
    set_return_data(&out);
    Ok(())
}
