# Compact diff of vanilla vs ours for an area: python3 xtask/dump_diff.py AREA [max]
import json,sys,re
a=sys.argv[1]; mx=int(sys.argv[2]) if len(sys.argv)>2 else 60
b='cache/mcdata/26.3/observed/commands-'
v=json.load(open(f'{b}{a}.json')); o=json.load(open(f'{b}{a}-ours.json'))
def s(c):
    if isinstance(c,dict):
        if 'translate' in c:
            w=[s(x) for x in c.get('with',[])]
            return c['translate']+('('+', '.join(w)+')' if w else '')
        if 'extra' in c and not c.get('text'): return ''.join(s(x) for x in c['extra'])
        if '' in c: return str(c[''])
        return c.get('text',json.dumps(c)[:60])
    return str(c)
def line(l):
    r=[('E:' if x['chat'].get('color')=='red' else '')+s(x['chat']) for x in l['replies'] if 'chat' in x]
    t=' | '.join(r)
    return re.sub(r'command.context.here|click_event[^|]*','',t)[:260]
n=0;same=0
for x,y in zip(v['lines'],o['lines']):
    p,q=line(x),line(y)
    if p==q: same+=1; continue
    n+=1
    if n<=mx: print('##',x['line'][:100]);print(' v',p);print(' o',q)
print(same,'same,',n,'differ')
