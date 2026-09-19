import re,json,sys,os
def load(path):
    t=open(path).read()
    m=re.search(r'const UNIT_DATA = (\[.*?\]);\s*\n',t,re.S)
    units=json.loads(m.group(1))
    return units
def summarize(path,top=15,ws_only=True):
    units=load(path)
    ws=[u for u in units if u.get('version') and (u['name'].startswith(('sovereign','corpus','commonwealth','kernel','oicp','oplog','xtask','arch','serving')))] if ws_only else units
    ws.sort(key=lambda u:-u['duration'])
    total=sum(u['duration'] for u in units)
    end=max((u['start']+u['duration']) for u in units) if units else 0
    print(f"{path.split('/')[-1]}: units={len(units)} ws_units={len(ws)} cpu_sum={total:.1f}s wall_end={end:.1f}s")
    for u in ws[:top]:
        rm=u.get('rmeta_time'); rm=f"{rm:.1f}" if rm else "-"
        print(f"   {u['duration']:6.1f}s  start={u['start']:6.1f} rmeta={rm:>5}  {u['name']} [{u['target']}] {u['mode']}")
    return units
if __name__=='__main__':
    for p in sys.argv[1:]: summarize(p, top=int(__import__('os').environ.get('TOP','15')))
