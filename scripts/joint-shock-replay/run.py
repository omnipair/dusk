"""Rebuild a source-hashed native replay and verify every insurance boundary.

The adapter is included only in a copied benchmark engine under target. It
never edits the program or substitutes AMM, underwriting or settlement math.
"""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent


def read_rows(path):
    return [json.loads(line) for line in path.read_text().splitlines()]


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2) + '\n')


def write_csv(path, rows):
    with path.open('w', newline='') as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]), lineterminator='\n')
        writer.writeheader()
        writer.writerows(rows)


def minimum_fund(losses):
    prior = 0
    required = 0
    for loss in losses:
        required = max(required, prior + 5 * loss)
        prior += loss
    return max(required, 2 * prior)


def run(output):
    output.mkdir(parents=True, exist_ok=True)
    marker = output / '.joint-shock-output'
    if any(output.iterdir()) and not marker.exists():
        raise RuntimeError('Output directory must be empty or created by this script')
    marker.touch()
    engine = output / 'engine-snapshot/programs/dusk'
    if engine.exists():
        shutil.rmtree(engine)
    shutil.copytree(ROOT / 'programs/dusk', engine)
    files = [{'path': str(p.relative_to(ROOT)), 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()}
             for p in sorted((ROOT / 'programs/dusk').rglob('*')) if p.is_file()]
    adapter = (HERE / 'stress_adapter.rs').read_bytes()
    (engine / 'src/stress_adapter.rs').write_bytes(adapter)
    with (engine / 'src/benchmark_api.rs').open('a') as stream:
        stream.write('\n// Off-chain stress adapter, excluded from the deployed program.\ninclude!("stress_adapter.rs");\n')
    with (engine / 'Cargo.toml').open('a') as stream:
        stream.write('\n[workspace]\n')
    shutil.copy2(ROOT / 'Cargo.lock', engine / 'Cargo.lock')
    shutil.copytree(HERE / 'native', output / 'native', dirs_exist_ok=True)
    manifest = {'head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                'program_files': files, 'adapter_sha256': hashlib.sha256(adapter).hexdigest(),
                'scenario_sha256': hashlib.sha256((HERE / 'scenarios.json').read_bytes()).hexdigest(),
                'scope': 'Host-native benchmark; not validator/SVM execution'}
    write_json(output / 'engine-manifest.json', manifest)
    shutil.copy2(HERE / 'native/Cargo.lock', output / 'native/Cargo.lock')
    env = dict(os.environ, CARGO_TARGET_DIR=str(ROOT / 'target/joint-shock-build'))
    with (output / 'build.log').open('w') as log:
        subprocess.run(['cargo', 'build', '--offline', '--manifest-path', str(output / 'native/Cargo.toml')],
                       cwd=ROOT, env=env, stdout=log, stderr=log, check=True)
    binary = ROOT / 'target/joint-shock-build/debug/dusk-joint-shock'
    subprocess.run([str(binary), '--regression-probes', str(output / 'regression-probes.json')], check=True)
    probes = json.loads((output / 'regression-probes.json').read_text())
    assert len(probes) == 32
    assert all(p['ok'] and p['start_gap_nad'] == [0, 0] for p in probes)
    assert max(abs(gap) for p in probes for gap in p['opposite_after_atoms']) <= 3

    def replay(name, configs):
        write_json(output / f'{name}.json', configs)
        with (output / f'{name}.log').open('w') as log:
            subprocess.run([str(binary), str(output / f'{name}.json'), str(output / f'{name}.jsonl')],
                           cwd=ROOT, stdout=log, stderr=log, check=True)
        return read_rows(output / f'{name}.jsonl')

    scenarios = replay('scenarios', json.loads((HERE / 'scenarios.json').read_text()))
    controls = []
    for name in ['baseline-funded', 'baseline-capital', 'diagnostics']:
        controls.extend(replay(name, json.loads((HERE / f'{name}.json').read_text())))
    assert len(scenarios) == 104
    assert sum(r['status'] == 'complete' for r in scenarios) == 102
    assert {r['id'] for r in scenarios if r['status'] != 'complete'} == {'admission-150000', 'admission-200000'}
    funded = replay('funded', [dict(r['config'], id=r['id'] + '-funded', insurance_usd=1e6)
                              for r in scenarios if r['status'] == 'complete' and r['losses']['shortfall_atoms'] > 0])
    boundaries = []
    for r in funded:
        assert r['status'] == 'complete' and r['losses']['socialized_atoms'] == 0
        required = int(r['path_implied_cap_reserve_atoms'])
        for delta in [0, -1]:
            c = dict(r['config'], id=r['id'] + f'-capital{delta}', insurance_atoms=required + delta)
            c.pop('insurance_usd', None)
            boundaries.append(c)
    capital = replay('capital-verification', boundaries)
    for r in capital:
        assert r['status'] == 'complete'
        assert (r['losses']['socialized_atoms'] == 0) == r['id'].endswith('capital0'), r['id']
    all_rows = scenarios + controls + funded + capital
    floors = 0
    for r in all_rows:
        if 'losses' not in r:
            continue
        losses = r['losses']
        assert losses['insurance_drawn_atoms'] + losses['socialized_atoms'] == losses['shortfall_atoms']
        events = [e for e in r['events'] if e['kind'] == 'floor']
        assert minimum_fund([e['shortfall_atoms'] for e in events]) == int(r['path_implied_cap_reserve_atoms'])
        for e in events:
            assert e['debt_atoms'] - min(e['swap_output_atoms'], e['debt_atoms']) == e['shortfall_atoms']
            assert e['insurance_atoms'] + e['socialized_atoms'] == e['shortfall_atoms']
            unit_value = e['external_price'] / 1e9 if r['config'].get('debt_asset') == 'base' else 1e-6
            allocation = sum(e[k] for k in ['ordinary_credit_loss_usd', 'base_hlp_credit_loss_usd', 'quote_hlp_credit_loss_usd'])
            assert abs(allocation - e['socialized_atoms'] * unit_value) < 1e-6
            floors += 1
    original = {r['id']: r for r in scenarios}
    insured = {r['id']: r for r in funded}
    reference = {float(r['sol_decline_pct']): r for r in csv.DictReader((HERE / 'analysis-reference.csv').open())}
    main = []
    for shock in [-.1, -.3, -.5, -.7]:
        r = original[f'h100000-s{shock}-w0-k0']
        f = insured.get(r['id'] + '-funded')
        required = int(f['path_implied_cap_reserve_atoms']) / 1e6 if f else 0
        main.append(dict(sol_decline_pct=-shock * 100,
                         uninsured_loss_usdc=r['losses']['socialized_usd'],
                         insurance_paid_usdc=f['losses']['insurance_drawn_usd'] if f else 0,
                         opening_insurance_usdc=required,
                         loss_change_from_analysis_usdc=r['losses']['socialized_usd'] - float(reference[-shock * 100]['uninsured_credit_loss_usdc']),
                         capital_change_from_analysis_usdc=required - float(reference[-shock * 100]['minimum_opening_insurance_usdc'])))
    write_csv(output / 'main-results.csv', main)
    capital_reference = {r['id']: r for r in csv.DictReader((HERE / 'analysis-capital-reference.csv').open())}
    requirements = []
    for r in funded:
        scale = 1e9 if r['config'].get('debt_asset') == 'base' else 1e6
        requirements.append(dict(id=r['id'], debt_asset='SOL' if scale == 1e9 else 'USDC',
                                 insurance_spend_tokens=r['losses']['insurance_drawn_atoms'] / scale,
                                 minimum_opening_tokens=int(r['path_implied_cap_reserve_atoms']) / scale,
                                 change_from_analysis_tokens=int(r['path_implied_cap_reserve_atoms']) / scale - float(capital_reference[r['id']]['minimum_opening_tokens'])))
    write_csv(output / 'insurance-requirements.csv', requirements)
    write_csv(output / 'scenario-summary.csv', [dict(id=r['id'], status=r['status'],
              liquidations=r.get('liquidated_positions'), credit_loss_usd=r.get('losses', {}).get('socialized_usd'),
              remaining_debt_usd=r.get('final', {}).get('remaining_debt_usd')) for r in scenarios])
    validation = dict(scenarios=len(scenarios), complete=102, admission_rejections=2, settlement_blocked=0,
                      funded_paths=len(funded), boundary_checks=len(capital), total_replays=len(all_rows),
                      focused_probes=len(probes), liquidations_reconciled=floors,
                      custody_and_preview_checks=sum(r.get('accounting_checks', 0) for r in all_rows))
    write_json(output / 'validation.json', validation)
    print(json.dumps({'validation': validation, 'main': main}, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'target/joint-shock-results')
    run(parser.parse_args().output.resolve())
