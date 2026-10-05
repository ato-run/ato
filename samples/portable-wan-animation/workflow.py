def compile_prompt(workflow, object_info):
    w = workflow
    oi = object_info
    nodes={n["id"]:n for n in w["nodes"]}
    links={l[0]:l for l in w["links"]}
    VIRT={"SetNode","GetNode","Reroute"}; SKIP={"Note","MarkdownNote"}
    setters={n["widgets_values"][0]:n for n in w["nodes"] if n["type"]=="SetNode"}
    def resolve(link_id):
        l=links[link_id]; src=nodes[l[1]]; slot=l[2]
        if src["type"]=="GetNode":
            s=setters[src["widgets_values"][0]]; return resolve(s["inputs"][0]["link"])
        if src["type"] in ("SetNode","Reroute"):
            return resolve(src["inputs"][0]["link"])
        return [str(src["id"]),slot]
    def is_widget(spec):
        t=spec[0]; opts=spec[1] if len(spec)>1 and isinstance(spec[1],dict) else {}
        if opts.get("forceInput"): return False
        return isinstance(t,list) or t in ("INT","FLOAT","STRING","BOOLEAN","COMBO")
    prompt={}
    for n in w["nodes"]:
        t=n["type"]
        if t in VIRT or t in SKIP or n.get("mode") in (2,4): continue
        info=oi[t]; inp={}
        specs={}
        for sec in ("required","optional"): specs.update(info["input"].get(sec,{}))
        wv=n.get("widgets_values")
        if isinstance(wv,dict):
            for k,v in wv.items():
                if k in specs: inp[k]=v
        elif isinstance(wv,list):
            vals=list(wv); i=0
            for name,spec in specs.items():
                if not is_widget(spec): continue
                if i>=len(vals): break
                inp[name]=vals[i]; i+=1
                opts=spec[1] if len(spec)>1 and isinstance(spec[1],dict) else {}
                if opts.get("control_after_generate") or name in ("seed","noise_seed"):
                    if i<len(vals) and vals[i] in ("fixed","randomize","increment","decrement"): i+=1
        for e in n.get("inputs",[]):
            if e.get("link") is not None: inp[e["name"]]=resolve(e["link"])
        for k,v in list(inp.items()):
            if isinstance(v,str) and "\\" in v: inp[k]=v.replace("\\","/")
        prompt[str(n["id"])]={"class_type":t,"inputs":inp}
    # --- adjustments for headless measurement
    prompt["22"]["inputs"]["attention_mode"]="sdpa"; prompt["22"]["inputs"].pop("compile_args",None)
    prompt.pop("35",None)
    src="animate"
    prompt["57"]["inputs"]["image"]=src+"_image.jpeg"
    prompt["63"]["inputs"]["video"]=src+"_video.mp4"; prompt["63"]["inputs"]["frame_load_cap"]=81
    for k in ("bg_images","mask"):
        prompt["62"]["inputs"].pop(k,None)
    
    return prompt
