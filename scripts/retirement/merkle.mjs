/** Ordered, domain-separated commitments to the compiled owner's account groups. */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
const hash = (...parts) => createHash('sha256').update(Buffer.concat(parts)).digest();
export function ownerEvidenceTree(digests) {
  assert.ok(digests.length > 0 && digests.length <= 8);
  const count = 2 ** Math.ceil(Math.log2(digests.length));
  let leaves = Array.from({ length: count }, (_, index) => {
    if (index >= digests.length) return hash(Buffer.from('DUSK_RETIREMENT_PAD_V1'), Buffer.from([index]));
    const value = Buffer.from(digests[index]); assert.equal(value.length, 32); return value;
  });
  const levels = [leaves];
  while (leaves.length > 1) {
    leaves = Array.from({ length: leaves.length / 2 }, (_, index) => hash(Buffer.from('DUSK_RETIREMENT_NODE_V1'), leaves[index * 2], leaves[index * 2 + 1]));
    levels.push(leaves);
  }
  return { root: [...leaves[0]], proofs: digests.map((_, index) => levels.slice(0, -1).map((level, depth) => [...level[(index >> depth) ^ 1]])) };
}
