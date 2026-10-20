# Compact view of an observed commands file: python3 xtask/dump_obs.py AREA [-ours] [from]
import json,sys
a=sys.argv[1]; ours=len(sys.argv)>2 and sys.argv[2]=='-ours'
v=json.load(open(f'cache/mcdata/26.3/observed/commands-{a}{"-ours" if ours else ""}.json'))
def s(c):
    if isinstance(c,dict):
        if 'translate' in c:
            w=[s(x) for x in c.get('with',[])]
            return c['translate']+('('+', '.join(w)+')' if w else '')
        if 'extra' in c and not c.get('text'): return ''.join(s(x) for x in c['extra'])
        if '' in c: return str(c[''])
        return c.get('text',json.dumps(c)[:60])
    return str(c)
for l in v['lines']:
    r=[('E:' if x['chat'].get('color')=='red' else '')+s(x['chat']) for x in l['replies'] if 'chat' in x]
    print(l['line'][:75],'=>',' | '.join(r)[:230])
