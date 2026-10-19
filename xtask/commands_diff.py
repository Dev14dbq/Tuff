# Compares observed/commands.json (vanilla) with commands-ours.json.
import json,sys
base='cache/mcdata/26.3/observed/'
v=json.load(open(base+'commands.json')); o=json.load(open(base+'commands-ours.json'))
def norm(x):
    # uuids, random numbers and timings differ by nature
    if isinstance(x,dict):
        return {k:norm(val) for k,val in x.items() if k not in ('uuid','insertion')}
    if isinstance(x,list): return [norm(i) for i in x]
    return x
same=0; diffs=[]
for a,b in zip(v['lines'],o['lines']):
    ra=[norm(r) for r in a['replies']]; rb=[norm(r) for r in b['replies']]
    if ra==rb: same+=1
    else: diffs.append((a['line'],ra,rb))
print(f"{same}/{len(v['lines'])} lines match")
for line,ra,rb in diffs[:int(sys.argv[1]) if len(sys.argv)>1 else 200]:
    print('##',line); print('  v',json.dumps(ra,ensure_ascii=False)[:400]); print('  o',json.dumps(rb,ensure_ascii=False)[:400])
