// Owned development fixtures. Runtime installation and hooks use the Rust CLI.
import fs from 'node:fs';
import path from 'node:path';
import net from 'node:net';
import http from 'node:http';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import {spawn,spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
export {fs,path,http,crypto,assert};
export const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../..');
export const sleep=ms=>new Promise(r=>setTimeout(r,ms));
export const sha=p=>crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');
export const token=()=>crypto.randomBytes(32).toString('hex');
export const quote=s=>"'"+String(s).replaceAll("'","'\\''")+"'";
export const sql=s=>"'"+String(s).replaceAll("'","''")+"'";
export const write=(p,v)=>{fs.mkdirSync(path.dirname(p),{recursive:true});fs.writeFileSync(p,typeof v==='string'||Buffer.isBuffer(v)?v:JSON.stringify(v,null,2)+'\n',{mode:0o600});};
export function command(cmd,args=[],options={}){const r=spawnSync(cmd,args.map(String),{cwd:root,encoding:'utf8',timeout:180000,maxBuffer:8*1024*1024,...options});if(r.error)throw r.error;if(!options.fail)assert.equal(r.status,0,r.stderr||r.stdout);else assert.notEqual(r.status,0);return r;}
export const jsonCommand=(cmd,args=[],options={})=>JSON.parse(command(cmd,args,options).stdout);
export const port=()=>new Promise((resolve,reject)=>{const s=net.createServer();s.on('error',reject);s.listen(0,'127.0.0.1',()=>{const p=s.address().port;s.close(()=>resolve(p));});});
export async function until(fn,limit=200){let last;for(let i=0;i<limit;i++){try{last=await fn();if(last)return last;}catch(e){last=e;}await sleep(50);}throw Error('Fixture deadline exceeded: '+String(last));}
export class Fixture {
 static async create(name,binary){const f=new Fixture();fs.mkdirSync(path.join(root,'.validation'),{recursive:true});f.out=fs.mkdtempSync(path.join(root,'.validation',name+'-'));f.project=path.join(f.out,'project');fs.mkdirSync(f.project);f.data=path.join(f.out,'data');f.config=path.join(f.out,'preflight.toml');f.base='http://127.0.0.1:'+await port();f.binary=path.resolve(binary||process.env.PREFLIGHT_BINARY||process.env.PF_SERVER_BINARY||path.join(root,'target',process.env.PREFLIGHT_TEST_PROFILE||'release','preflight'));f.children=[];f.closers=[];f.env={...process.env};f.session=null;f.serial=0;f.writeConfig();return f;}
 writeConfig(extra='',roots=[this.project]){write(this.config,`version=1\nbind=${JSON.stringify(this.base.slice(7))}\ndata_dir=${JSON.stringify(this.data)}\nallowed_roots=${JSON.stringify(roots)}\nmax_parallel_commands=4\n`+extra);}
 spawn(cmd,args=[],name='process',env=this.env){const fd=fs.openSync(path.join(this.out,name+'.log'),'a',0o600);const p=spawn(String(cmd),args.map(String),{cwd:root,env,detached:process.platform!=='win32',stdio:['ignore',fd,fd]});fs.closeSync(fd);this.children.push(p);p.on('error',e=>{p.fixtureError=e;});return p;}
 async ready(url,p){await until(async()=>{assert.equal(p.exitCode,null,'Owned process exited');if(p.fixtureError)throw p.fixtureError;try{return (await fetch(url,{signal:AbortSignal.timeout(1000)})).ok;}catch{return false;}});}
 async start(){this.session=null;this.server=this.spawn(this.binary,['--config',this.config,'serve'],'server');await this.ready(this.base+'/health',this.server);}
 signal(p,signal){try{if(process.platform==='win32')p.kill(signal);else process.kill(-p.pid,signal);}catch(e){if(e.code!=='ESRCH')throw e;}}
 async stop(p=this.server){if(!p||p.exitCode!==null)return;const done=new Promise(r=>p.once('exit',r));this.signal(p,'SIGTERM');let timer;await Promise.race([done,new Promise(r=>{timer=setTimeout(()=>{this.signal(p,'SIGKILL');r();},10000);})]);clearTimeout(timer);await done;}
 async cleanup(){for(const fn of this.closers.reverse())await fn();for(const p of [...this.children].reverse())await this.stop(p);}
 async response(route,body,headers={},method){const r=await fetch(this.base+route,{method:method||(body===undefined?'GET':'POST'),headers:{'Content-Type':'application/json',...headers},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(30000)});return r;}
 async call(route,body,headers={}){const r=await this.response(route,body,headers);assert(r.ok,`HTTP ${r.status}: ${await r.clone().text()}`);return r.json();}
 api(route,body,headers={}){return this.call('/api/v1/'+route,body,headers);}
 async register(name='Owned acceptance',directory=this.project){fs.mkdirSync(directory,{recursive:true});return this.api('projects',{name,path:directory});}
 async run(project,name,definition,revision=0){await this.api('pipeline-configs',{project_id:project,name,expected_revision:revision,definition});return this.api('saved-pipeline-runs',{project_id:project,name,revision:revision+1});}
 async finished(id,status){const result=await until(async()=>{const r=await this.api('runs/'+id);return r.finished_at!==null?r:null;},600);if(status)assert.equal(result.status,status,JSON.stringify(result));return result;}
 async client(token){const base='http://127.0.0.1:'+await port();const args=['--server',this.base,'--bind',base.slice(7)];const env={...this.env};if(token){args.push('--token-env','PREFLIGHT_API_TOKEN');env.PREFLIGHT_API_TOKEN=token;}const p=this.spawn(process.env.PREFLIGHT_CLIENT_BINARY||path.join(root,'target',process.env.PREFLIGHT_TEST_PROFILE||'release','preflight-client'),args,'client-'+this.children.length,env);await this.ready(base,p);return base;}
 async rpc(method,params,notify=false,headers={}){const id=++this.serial;const r=await this.response('/mcp',{jsonrpc:'2.0',method,...(params===undefined?{}:{params}),...(notify?{}:{id})},{Accept:'application/json, text/event-stream',...(this.session?{'Mcp-Session-Id':this.session}:{}),...headers});assert(r.ok,'MCP HTTP '+r.status);this.session=r.headers.get('Mcp-Session-Id')||this.session;if(r.status===202)return null;if(r.headers.get('content-type')?.startsWith('application/json'))return r.json();let text='',size=0;const reader=r.body.getReader();const decoder=new TextDecoder();try{while(true){const {value,done}=await reader.read();if(done)break;size+=value.length;assert(size<=2*1024*1024,'MCP response exceeds limit');text+=decoder.decode(value,{stream:true});let end;while((end=text.indexOf('\n\n'))>=0){const block=text.slice(0,end);text=text.slice(end+2);const payload=block.split('\n').filter(l=>l.startsWith('data:')).map(l=>l.slice(5).trimStart()).join('\n');if(payload){const value=JSON.parse(payload);if(value.id===id)return value;}}}}finally{await reader.cancel();}throw Error('MCP response missing');}
 async initialize(headers={}){const r=await this.rpc('initialize',{protocolVersion:'2025-06-18',capabilities:{},clientInfo:{name:'owned-shell-acceptance',version:'1'}},false,headers);assert(!r.error);await this.rpc('notifications/initialized',undefined,true,headers);}
 async tool(name,args,fail=false,headers={}){const r=await this.rpc('tools/call',{name,arguments:args},false,headers);assert(!r.error,JSON.stringify(r));if(fail){assert(r.result.isError);return JSON.stringify(r.result);}assert(!r.result.isError,JSON.stringify(r.result));return JSON.parse(r.result.content.find(c=>c.type==='text').text);}
 async browser(script,fixture,args=[],env={}){const input=path.join(this.out,'fixture.json');write(input,fixture);await this.browserArgs(script,[input,this.out,...args],env);}
 async browserArgs(script,args,env={}){const p=this.spawn(process.execPath,[path.join(root,'scripts',script),...args.map(String)],'browser',{...this.env,...env});let timer;await new Promise((resolve,reject)=>{timer=setTimeout(()=>{this.signal(p,'SIGTERM');reject(Error('Browser deadline exceeded: '+this.out));},240000);p.once('error',reject);p.once('exit',code=>code===0?resolve():reject(Error('Browser failed: '+this.out+'/browser.log')));}).finally(()=>clearTimeout(timer));}
 receipt(value){write(path.join(this.out,'acceptance.json'),value);console.log(JSON.stringify({out:this.out,...value}));}
}
export const pipeline=(script='true',id='build')=>({version:1,pipeline:{stages:[{id,command:{program:'sh',args:['-c',script],timeout_seconds:30}}]}});
export async function withFixture(name,fn,binary){const f=await Fixture.create(name,binary);try{await fn(f);}finally{await f.cleanup();}}
