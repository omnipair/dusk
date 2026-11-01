# Devnet tranche deployment

Dusk PR #61 source commit `c8ef8e32302ee88ce99c1244d5605892ca61aa80`
was built after the complete local CI gate. The previous Dusk deployment was
closed during the authorized fresh-start rollout, returning `25.11083116` SOL
of devnet rent. Its replacement was deployed at finalized slot `517399592`
under program id `5EuU8artxWthfER6GTUY52TfL1aRsQH2TH8DsdGCa8cJ`.
The leverage delegate kept its program id and was upgraded at finalized slot
`517399748` to bind the replacement Dusk program.

Both ProgramData binaries match their local artifacts byte for byte. Both
published IDLs match their generated local files after canonical JSON ordering.
The explicit deployment buffers were closed automatically, leaving no buffer
rent behind. The upgrade authority remains
`FJWRK3XJeVD8njSvTFXyHwP2jkatvBqeVwcNAFe5zVfJ`.

See `deployment.json` for the exact hashes, slots and transaction signatures.
