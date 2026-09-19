import sys,os
sys.path.insert(0,os.path.dirname(__file__))
from parse_timing import load
def critpath(path):
    u=load(path)
    byi={x['i']:x for x in u}
    pred={}
    for x in u:
        end=x['start']+x['duration']
        for j in x.get('unblocked_units',[]):
            if j not in pred or end>pred[j][0]: pred[j]=(end,x['i'])
        rm=x.get('rmeta_time')
        for j in x.get('unblocked_rmeta_units',[]):
            t=x['start']+(rm if rm else x['duration'])
            if j not in pred or t>pred[j][0]: pred[j]=(t,x['i'])
    last=max(u,key=lambda x:x['start']+x['duration'])
    out=[];cur=last['i']
    while cur is not None:
        x=byi[cur]; out.append(x); cur=pred.get(cur,(None,None))[1]
    ws=[x for x in u if x['name'].startswith(('sovereign','corpus','commonwealth','kernel','oicp','oplog','xtask','arch','serving'))]
    print(f"== {os.path.basename(path)}: {len(u)} units ({len(ws)} workspace), wall {last['start']+last['duration']:.1f}s, cpu {sum(x['duration'] for x in u):.1f}s")
    print("   critical path:")
    for x in reversed(out):
        rm=x.get('rmeta_time'); rm=f" rmeta@{rm:.1f}" if rm else ""
        print(f"   {x['start']:6.1f} +{x['duration']:5.1f}{rm}  {x['name']}{x['target']} [{x['mode']}]")
    print("   workspace units rebuilt (sorted by duration):")
    for x in sorted(ws,key=lambda x:-x['duration'])[:12]:
        print(f"   {x['duration']:6.1f}s  {x['name']}{x['target']} [{x['mode']}]")
if __name__=='__main__':
    for p in sys.argv[1:]: critpath(p)
