# Sample command lines for every executable branch of the vanilla command tree.
#   python3 xtask/commands/gen_leaves.py xtask/commands/leaves_names.json xtask/commands/leaves.tsv
# Then: COMMAND_LEAVES=xtask/commands/leaves.tsv cargo test -p mc-server commands_reach_a_handler -- --ignored --nocapture
import json,sys
t=json.load(open('cache/mcdata/26.3/reports/commands.json'))
sample={
'minecraft:entity':'@e','minecraft:game_profile':'Steve','brigadier:integer':'1','brigadier:float':'1.0','brigadier:double':'1.0','brigadier:bool':'true',
'brigadier:string':'a','minecraft:resource_key':'minecraft:overworld','minecraft:resource':'minecraft:stone','minecraft:resource_location':'minecraft:stone',
'minecraft:message':'hi','minecraft:component':'"a"','minecraft:item_predicate':'minecraft:stone','minecraft:block_pos':'0 64 0','minecraft:block_predicate':'minecraft:stone',
'minecraft:dimension':'minecraft:overworld','minecraft:context_float_provider':'minecraft:brewing/speed_default','minecraft:context_int_provider':'minecraft:brewing/uses_default','minecraft:vec3':'0 64 0','minecraft:nbt_path':'a',
'minecraft:nbt_compound_tag':'{}','minecraft:nbt_tag':'1','minecraft:function':'minecraft:f','minecraft:gamemode':'survival','minecraft:dialog':'minecraft:x','minecraft:swizzle':'xyz',
'minecraft:entity_anchor':'feet','minecraft:resource_or_tag':'minecraft:stone','minecraft:slot_source':'container.0','minecraft:loot_predicate':'minecraft:x','minecraft:score_holder':'a',
'minecraft:objective':'o','minecraft:int_range':'1','minecraft:float_range':'1','minecraft:heightmap':'world_surface','minecraft:rotation':'0 0','minecraft:uuid':'00000000-0000-0000-0000-000000000000',
'minecraft:block_state':'minecraft:stone','minecraft:column_pos':'0 0','minecraft:item_stack':'minecraft:stone','minecraft:loot_modifier':'minecraft:x','minecraft:resource_or_tag_key':'minecraft:stone',
'minecraft:loot_table':'minecraft:x','minecraft:item_slot':'container.0','minecraft:particle':'minecraft:flame','minecraft:feature':'minecraft:oak','minecraft:template_rotation':'none','minecraft:template_mirror':'none',
'minecraft:time':'1t','minecraft:objective_criteria':'dummy','minecraft:style':'{}','minecraft:scoreboard_slot':'sidebar','minecraft:operation':'=','minecraft:vec2':'0 0','minecraft:swing_animation':'mainhand',
'minecraft:team':'t','minecraft:team_color':'red','minecraft:resource_selector':'minecraft:*','minecraft:hex_color':'ffffff'}
byname=json.load(open(sys.argv[1])) if len(sys.argv)>1 else {}
def val(root,k,v):
    if v['parser']=='minecraft:entity':
        pr=v.get('properties',{}); one=pr.get('amount')=='single'; pl=pr.get('type')=='players'
        return ('@p' if pl else '@e[limit=1]') if one else ('@a' if pl else '@e')
    if v['parser'] in ('minecraft:resource_location','minecraft:resource','minecraft:resource_key') and k in ('target','source','entity','targets','id'):
        return sample[v['parser']]
    key=root+'.'+k
    if (root,k)==('summon','entity'): return 'minecraft:pig'
    if (root,k)==('fillbiome','filter'): return 'minecraft:plains'
    if (root,k)==('summon','entity'): return 'minecraft:pig'
    ok = v['parser'] in ('minecraft:entity','minecraft:resource_location','minecraft:resource','minecraft:resource_key','minecraft:game_profile','minecraft:resource_or_tag','minecraft:loot_table')
    if ok and key in byname: return byname[key]
    if ok and k in byname: return byname[k]
    return sample.get(v['parser'],'?')
lines=[]
def walk(n,path,toks,root):
    if n.get('executable'): lines.append((' '.join(path),' '.join(toks)))
    for k,v in n.get('children',{}).items():
        if 'redirect' in v and not v.get('children'): continue
        if v['type']=='literal': walk(v,path+[k],toks+[k],root)
        else: walk(v,path+['<'+k+'>'],toks+[val(root,k,v)],root)
for r,n in sorted(t['children'].items()):
    if 'redirect' in n or r in ('stop','execute'): continue
    walk(n,[r],[r],r)
open(sys.argv[2] if len(sys.argv)>2 else 'out.tsv','w').write('\n'.join(a+'\t'+b for a,b in lines))
