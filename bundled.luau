local a={cache={}}do do local function b()local c=require'@lune/stdio'local function d(e,f)return c.color(f)..e..c.color
'reset'end local e={}function e.format(f,g,h)return`{d(f,g)} {h}`end function e.info(f)print(e.format('INFO','blue',f))
end function e.warn(f)print(e.format('WARN','yellow',f))end function e.success(f)print(e.format('OK  ','green',f))end
function e.error(f)print(e.format('ERR ','red',f))end return e end function a.a()local c=a.cache.a if not c then c={c=b(
)}a.cache.a=c end return c.c end end do local function b()local c,d,e,f,g,h,i=require'@lune/fs',require'@lune/task',
require'@lune/roblox',require'@lune/process',require'@lune/stdio',a.a(),{}i.NAME='rojo-build-noalias'i.VERSION='1.0.0'i.
TEMP_OUTPUT_FILENAME='/tmp/rojo-build-noalias/build.rbxm'local j=string.gsub(i.TEMP_OUTPUT_FILENAME,'/[^/]*$','')c.
writeDir(j)function i.formatHelp(k)k=string.gsub(k,'^%s+\n','')k=string.gsub(k,'%s+$','')local l,m=string.split(k,'\n'),
0 for n,o in l do if n==1 then m=o:match'^ +':len()end l[n]=string.sub(o,m+1,-1)end table.insert(l,1,g.color'green'..i.
NAME..g.color'reset'..' '..i.VERSION)local n=string.format(table.concat(l,'\n'),i.VERSION)n=string.gsub(n,'[%u ]+:',g.
color'yellow'..'%1'..g.color'reset')n=string.gsub(n,'<[%u]>',g.color'green'..'%1'..g.color'reset')n=string.gsub(n,
'(%-%a+),',g.color'green'..'%1'..g.color'reset'..',')n=string.gsub(n,'(%-%-%a+)',g.color'green'..'%1'..g.color'reset')
return n end function i.watch(k,l,m)local n=k[l]while true do local o=n:read()if not o then break end d.spawn(m,o)end
end function i.processArgs(k)local l,m='',table.clone(k)table.insert(m,1,'build')for n,o in m do if o~='-o'and o~=
'--output'then continue end local p=m[n+1]if not p then h.error
[[You must specify an output filename for the build with either '-o' or '--output']]return end l=p table.remove(m,n)
table.remove(m,n)table.insert(m,n,'-o')table.insert(m,n+1,i.TEMP_OUTPUT_FILENAME)break end return m,l end local k={'-',
'+','*','[',']','(',')'}function i.patternSafeString(l)for m,n in k do l=string.gsub(l,`%{n}`,`%%{n}`)end return l end
function i.tryGetFolderPath(l,m)local n=m or 1 local o=n if not l.children then return end local p for q,r in l.children
do if r.filePaths then p=r else p,n=i.tryGetFolderPath(r,o+1)end if p then break end end if o~=1 then return p,n elseif
p then local q=p.filePaths[1]local r=string.split(q,'/')for s=1,n do table.remove(r)end return table.concat(r,'/')end
return nil end function i.getFullInstancePath(l)local m,n={},l while true do if n==nil then break end table.insert(m,n.
Name)n=n.Parent end local o=#m for p=o-1,1,-1 do m[o]=table.remove(m,p)end return m end function i.getFullChildPath(l,m)
local n={}for o,p in m do table.insert(n,p.name)end return n end function i.getRelativePath(l,m)local n,o=1,math.min(#l,
#m)while n<=o and l[n]==m[n]do n=n+1 end local p=n-1 local q=#l-p-1 if q<0 then q=0 end local r={}if q==0 then table.
insert(r,'.')else for s=1,q do table.insert(r,'..')end end if p+1<=#m then for s=p+1,#m do table.insert(r,m[s])end end
return r end function i.forEachInChild(l,m,n)local o=n and table.clone(n)or{}d.spawn(m,l,o)if not l.children then return
end table.insert(o,l)for p,q in l.children do i.forEachInChild(q,m,o)end end function i.forEachInSourcemap(l,m)if not l.
children then return end for n,o in l.children do i.forEachInChild(o,m,{l})end end return i end function a.b()local c=a.
cache.b if not c then c={c=b()}a.cache.b=c end return c.c end end end local b,c,d,e,f,g,h,i=require'@lune/fs',require
'@lune/task',require'@lune/stdio',require'@lune/serde',require'@lune/roblox',require'@lune/process',a.a(),a.b()if#g.args
==0 or table.find(g.args,'-h')or table.find(g.args,'--help')then local j=
[[    A wrapper around the Rojo build command.

    USAGE:
        rojo-build-noalias [OPTIONS] [PROJECT]

    ROJO ARGS:
        <PROJECT>   Path to the project to serve. Defaults to the current directory [default: ]

    ROJO OPTIONS:
            --color <COLOR>     Set color behavior. Valid values are auto, always, and never [default: auto]
        -h, --help              Print help information
        -o, --output <OUTPUT>   Where to output the result
        -v, --verbose           Sets verbosity level. Can be specified multiple times
            --watch             Whether to automatically rebuild when any input files change
    ]]
print(i.formatHelp(j))g.exit()end if table.find(g.args,'--plugin')then h.error
"The '--plugin' option is not currently supported."g.exit()end local j,k=i.processArgs(g.args)if not j or not k then
return end local l=g.create('rojo',j)c.spawn(i.watch,l,'stderr',function(m)d.write(h.format('ROJO','purple',m))end)i.
watch(l,'stdout',function(m)d.write(h.format('ROJO','purple',m))if not string.find(m,'Built project to')then return end
local n,o,p=os.clock(),pcall(function()local n=g.exec('rojo',{'sourcemap'})if not n.ok then h.error(`Rojo sourcemap generation failed with exit code {
n.code} and error:\n{n.stderr}`)return end local o,p=e.decode('json',n.stdout),e.decode('json',b.readFile'.luaurc')local
q=(p.aliases)if not q then return end local r={}i.forEachInSourcemap(o,function(s,t)local u if s.filePaths then u=s.
filePaths[1]else u=i.tryGetFolderPath(s)end if not u then h.warn(`Folder '{i.getFullChildPath(s,t)}' cannot be mapped because it doesn't have any children!`
)return end r[u]={path=u,child=s,crumbs=t}end)local s={}for t,u in q do local v=string.gsub(u,'^%./','')local w=r[v]or r
[v..'/init.luau']or r[v..'/init.lua']if not w then h.warn(`Alias '{t}' with path '{u}' was not found in the sourcemap. Requires using this alias won't be replaced.`
)continue end s[t]=w.path end local t=f.deserializeModel(b.readFile(i.TEMP_OUTPUT_FILENAME))local u,v=t[1],{}for w,x in
u:GetDescendants()do if not x:IsA'LuaSourceContainer'then continue end local y,z,A=i.getFullInstancePath(x),x.Source,
false for B,C in s do local D,E,F=r[C],{{true,`require%(["']@{i.patternSafeString(B)}["']%)`},{false,`require%(["']@{i.
patternSafeString(B)}/([^"']+)["']%)`}}for G,H in E do local I,J=H[1],H[2]z=string.gsub(z,J,function(K)A=true if not F
then F=i.getFullChildPath(D.child,D.crumbs)end local L,M=(table.concat(i.getRelativePath(y,F),'/'))if I then M=`require("{
L}")`else M=`require("{L}/{K}")`end table.insert(v,{table.concat(y,'/'),{`require("{I and`@{B}`or`@{B}/{K}`}")`,M}})
return M end)end end if A then x.Source=z end end do table.sort(v,function(w,x)return#w[2][1]<#x[2][1]end)local w,x=0,0
for y,z in v do w=math.max(string.len(z[1]),w)x=math.max(string.len(z[2][1]),x)end for y,z in v do h.info(z[1]..string.
rep(' ',w-string.len(z[1]))..' '..z[2][1]..string.rep(' ',x-string.len(z[2][1]))..' -> '..z[2][2])end end local w=f.
serializeModel{u}b.writeFile(k,w)h.success(`Built project to {k}`)return true end)if o and p==true then local q=math.
round((os.clock()-n)*1000*100)/100 h.info(`Done in {q}ms\n`)elseif not o then h.warn(`Project build failed because an error occurred:\n{
p}`)end end)