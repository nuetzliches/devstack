import sys
def apply(path, reps):
    s=open(path,encoding='utf-8').read()
    for a,b in reps:
        n=s.count(a)
        if n!=1:
            print("COUNT",n,"for:\n"+a[:300]); sys.exit(1)
        s=s.replace(a,b)
    open(path,'w',encoding='utf-8',newline='').write(s)
