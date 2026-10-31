"""Independent high precision checks of tiny, signed ownership movements."""
import csv
import runpy
from pathlib import Path
here = Path(__file__).resolve().parent
reference = runpy.run_path(str(here / 'generate-reference.py'))['reference']
with (here / 'local-invariant-reference.csv').open('w') as f:
    out = csv.writer(f)
    out.writerow(['amp','core','fade','lambda','oracle','cap','x0','y0','x1','y1','delta_floor_q64','price_floor_nad'])
    start = (10**10, 600*10**9)
    for amp,core,fade in [(1,0,0),(4,1000,4000),(32,100,0),(2000,1,0)]:
        for lam in [1,2500,9999]:
            for oracle in [6,60,600]:
                for dx,dy in [(1,-1),(-1,1),(100,-400),(-1000,2000),(5_000_000,-300_000_000),(-5_000_000,300_000_000)]:
                    end = (start[0]+dx,start[1]+dy)
                    out.writerow([amp,core,fade,lam,oracle,2*10**9,*start,*end,
                        *reference(amp,core,fade,lam,oracle,2*10**9,start,end)])
                    f.flush()
