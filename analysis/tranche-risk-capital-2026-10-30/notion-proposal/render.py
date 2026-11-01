"""Render explanatory figures from the proposal's illustrative assumptions."""
from pathlib import Path
from html import escape
import base64
import json
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import numpy as np

OUT=Path(__file__).resolve().parent
BRAND=OUT.parents[2]/'assets'/'brand'
CONFIG=json.loads((BRAND/'omnipair-light.json').read_text())
def token(path):
    family,key=path.split('.')
    return CONFIG['tokens'][family][key]
def role(name):
    return token(CONFIG['light_roles'][name])
BG=role('background'); INK=role('ink'); MUTED=role('muted')
ORANGE=role('accent'); BLUE=role('primary_text'); LINE=role('border')
FONT_DATA=base64.b64encode((BRAND/CONFIG['typography']['font_file']).read_bytes()).decode()
FONT_FACE=f"@font-face{{font-family:'M Saans';src:url(data:font/woff2;base64,{FONT_DATA}) format('woff2');font-weight:100 900;font-style:normal;font-display:block;}}"
SVG_STYLE=f"<style>{FONT_FACE}text{{font-family:'M Saans',Arial,sans-serif!important}}</style>"
parts=[f'<svg xmlns="http://www.w3.org/2000/svg" width="1440" height="1020" viewBox="0 0 1440 1020"><defs>{SVG_STYLE}<pattern id="reserve" width="10" height="10" patternUnits="userSpaceOnUse"><path d="M-2 2L2 -2M0 10L10 0M8 12L12 8" stroke="{LINE}" stroke-width="1"/></pattern></defs><rect width="1440" height="1020" fill="{BG}"/>']
def text(x,y,value,size=22,fill=INK,weight=400):
    parts.append(f'<text x="{x}" y="{y}" font-family="M Saans, Arial, sans-serif" font-size="{size}" font-weight="{weight}" fill="{fill}">{escape(value)}</text>')
def rect(x,y,w,h,fill,r=4,stroke='none'):
    parts.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{fill}" stroke="{stroke}" stroke-width="1.5"/>')
def line(x1,y1,x2,y2,color=LINE):
    parts.append(f'<path d="M{x1} {y1} L{x2} {y2}" stroke="{color}" stroke-width="1.5" fill="none"/>')
def arrow(x,y):
    parts.append(f'<path d="M{x} {y} h38 m-8 -7 l8 7 -8 7" fill="none" stroke="{MUTED}" stroke-width="1.5"/>')
text(72,64,'CAPITAL STRUCTURE',15,MUTED,500)
text(72,130,'Protection from capital already in the pool',43,INK,550)
text(72,173,'Same $100 capital budget. Illustrative allocations, not current pool balances.',22,MUTED)
line(72,214,1368,214)
text(72,280,'01',18,MUTED)
text(124,281,'Current approach',26,INK,550)
text(124,316,'Pool liquidity and a separate insurance reserve',21,MUTED)
rect(124,345,985.6,72,token('neutral.200'))
rect(1121.6,345,246.4,72,'url(#reserve)',4,LINE)
text(148,389,'$80 productive pool',24,INK,550)
text(1143.6,389,'$20 insurance',23,INK,500)
text(124,451,'Supports trading and borrowing',18,MUTED)
text(1121.6,451,'Outside the AMM',18,MUTED)
text(72,527,'02',18,MUTED)
text(124,528,'Proposed approach',26,INK,550)
text(124,563,'Both tranches remain inside the productive pool',21,MUTED)
rect(124,592,985.6,72,token('neutral.200'))
rect(1121.6,592,246.4,72,INK)
text(148,636,'$80 senior',24,INK,550)
text(1143.6,636,'$20 junior',24,token('neutral.50'),550)
text(124,698,'Junior covers lending losses first',18,MUTED)
text(1121.6,698,'Takes losses first',18,MUTED)
line(72,744,1368,744)
text(72,792,'When a loan leaves bad debt',25,INK,550)
for x,n,label,sub in [(72,'01','Recover collateral','Borrower collateral and position equity'),(544,'02','Junior absorbs the shortfall','Up to its remaining capital'),(1040,'03','Senior absorbs any excess','Only after junior is exhausted')]:
    text(x,842,n,16,MUTED)
    text(x,877,label,22,INK,550)
    text(x,910,sub,17,MUTED)
arrow(458,868);arrow(952,868)
text(72,979,'Changing the loss order creates protection, not spendable cash. Token withdrawals still need available liquidity.',18,MUTED)
parts.append('</svg>')
(OUT/'capital-and-losses.svg').write_text(''.join(parts))

plt.rcParams.update({'font.family':'DejaVu Sans','font.size':12,'svg.fonttype':'none'})
fig,ax=plt.subplots(figsize=(13.5,7.2),dpi=160)
fig.patch.set_facecolor(BG);ax.set_facecolor(BG)
x=np.linspace(0,100,101);y=4+0.12*x
ax.plot(x,y,color=INK,linewidth=2.8,zorder=3)
ax.fill_between(x,4,y,color=LINE,alpha=.35)
ax.axhline(4,color=MUTED,linewidth=1.4,linestyle=(0,(4,4)))
for xx in [0,10,30,50,100]:
    yy=4+.12*xx
    ax.scatter(xx,yy,s=65,color=INK,edgecolor=BG,linewidth=1.8,zorder=4)
    offset=(10,13) if xx<100 else (-49,-25)
    if xx==0:offset=(10,-25)
    ax.annotate(f'{yy:.1f}%',(xx,yy),xytext=offset,textcoords='offset points',fontsize=14,color=INK,fontweight='medium')
ax.set_xlim(-2,104);ax.set_ylim(2,18)
ax.set_xticks([0,10,30,50,75,100],[f'{i}%' for i in [0,10,30,50,75,100]])
ax.set_yticks([4,8,12,16],[f'{i}%' for i in [4,8,12,16]])
ax.set_xlabel('Your deposit allocated to junior',labelpad=15,color=INK)
ax.set_ylabel('Combined interest APR',labelpad=15,color=INK)
ax.tick_params(colors=MUTED,length=0,pad=9)
ax.grid(axis='y',color=LINE,linewidth=.7)
for spine in ax.spines.values():spine.set_visible(False)
fig.text(.08,.94,'Choose an allocation between the two yields',fontsize=24,color=INK,weight='medium')
fig.text(.08,.89,'Illustrative quotes: senior 4% APR, junior 16% APR. No compounding.',fontsize=12,color=MUTED)
fig.text(.08,.06,'Quotes held fixed for a small position. Actual rates change with the pool. Swap fees and losses are excluded.',fontsize=10,color=MUTED)
fig.subplots_adjust(left=.09,right=.95,top=.81,bottom=.20)
fig.savefig(OUT/'allocation-yield.svg',facecolor=BG)
fig.savefig(OUT/'allocation-yield.png',facecolor=BG)
plt.close(fig)
chart=(OUT/'allocation-yield.svg').read_text()
(OUT/'allocation-yield.svg').write_text(chart.replace('</defs>',SVG_STYLE+'</defs>',1))
variables={
    'bg':BG,'ink':INK,'muted':MUTED,'line':LINE,'orange':ORANGE,
    'blue':BLUE,'cream':role('surface'),'accent-text':role('accent_text'),
    'accent-surface':role('accent_surface'),'accent-border':LINE,
    'primary-surface':role('primary_surface'),'secondary-surface':role('secondary_surface'),
    'secondary-text':role('secondary_text'),'focus':INK,
}
css=':root{'+''.join(f'--{k}:{v};' for k,v in variables.items())+'}'
logo='data:image/svg+xml;base64,'+base64.b64encode((BRAND/CONFIG['logo']['file']).read_bytes()).decode()
html=(OUT/'allocator.template.html').read_text().replace('{{BRAND_VARIABLES}}',css).replace('{{BRAND_FONT}}',FONT_FACE).replace('{{BRAND_LOGO}}',logo)
(OUT/'allocator.html').write_text(html)
print('Rendered proposal visuals with Omnipair light tokens and embedded M Saans.')
