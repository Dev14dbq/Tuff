# Compares what vanilla said (observed/commands[-AREA].json) with what our
# server said (commands[-AREA]-ours.json): `python3 xtask/commands_diff.py
# [AREA] [how many differences to show]`.
import json,sys
base='cache/mcdata/26.3/observed/'
args=sys.argv[1:]
area=''
if args and not args[0].isdigit():
    area='-'+args.pop(0)
show=int(args[0]) if args else 200
v=json.load(open(base+f'commands{area}.json')); o=json.load(open(base+f'commands{area}-ours.json'))
def norm(x):
    # uuids and random numbers differ by nature
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
for line,ra,rb in diffs[:show]:
    print('##',line); print('  v',json.dumps(ra,ensure_ascii=False)[:600]); print('  o',json.dumps(rb,ensure_ascii=False)[:600])
