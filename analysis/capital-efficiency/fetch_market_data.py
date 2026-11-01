"""Fetch checksum-verified Binance public spot archives for this experiment."""
from datetime import datetime, timezone
from pathlib import Path
import hashlib
import json
import urllib.request

ROOT=Path(__file__).resolve().parent/'market-data'
BASE='https://data.binance.vision/data/spot/'
FILES=[
 ('monthly/klines/SOLUSDT/1m/','SOLUSDT-1m-2025-01.zip'),
 *[('daily/klines/SOLUSDT/1s/',f'SOLUSDT-1s-{date}.zip') for date in ['2025-01-08','2025-01-09','2025-01-15','2025-01-19']],
 ('daily/aggTrades/SOLUSDT/','SOLUSDT-aggTrades-2025-01-01.zip')]


def fetch():
    ROOT.mkdir(exist_ok=True)
    sources=[]
    for folder,name in FILES:
        url=BASE+folder+name
        target=ROOT/name
        if not target.exists():
            print('Downloading',name,flush=True)
            with urllib.request.urlopen(url,timeout=45) as response:
                body=response.read()
            target.write_bytes(body)
        checksum_path=ROOT/(name+'.CHECKSUM')
        if not checksum_path.exists():
            with urllib.request.urlopen(url+'.CHECKSUM',timeout=45) as response:
                checksum_path.write_bytes(response.read())
        expected=checksum_path.read_text().split()[0]
        actual=hashlib.sha256(target.read_bytes()).hexdigest()
        if expected!=actual:
            raise ValueError('Publisher checksum mismatch: '+name)
        sources.append({'file':name,'url':url,'checksum_url':url+'.CHECKSUM','sha256':actual,'bytes':target.stat().st_size})
        print('Verified',name,target.stat().st_size,'bytes',flush=True)
    manifest={'retrieved_at':datetime.now(timezone.utc).isoformat(),'sources':sources,
        'documentation':'https://github.com/binance/binance-public-data/blob/master/README.md',
        'period':'January 2025, historical sample; not current performance',
        'access_note':'Attempt to retrieve August 2026 monthly data returned HTTP 404; no local historical dataset existed. January 2025 is an explicit historical test period.',
        'selection':'Five hourly windows selected from January 2–31 minute data: variance percentiles 10,50,90,99 and largest absolute hourly return. Selection is independent of simulated LP outcomes.',
        'training':'January 1 aggregated-trade clips supply a size proxy; no Dusk customer demand or routing behavior is observed.'}
    (ROOT/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    return manifest


if __name__=='__main__':
    fetch()
