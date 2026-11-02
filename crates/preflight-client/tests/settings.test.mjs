import test from 'node:test';
import assert from 'node:assert/strict';
import {settingsGroups,validateSettings,validatePolicies,settingsEqual} from '../src/settings.mjs';
const base={bind:'127.0.0.1:4170',data_dir:'data',allowed_roots:['examples'],max_active_runs:32,max_parallel_commands:2,default_stage_memory_mib:256};
test('every editable server field has a format, explanation and example',()=>{
 const fields=settingsGroups.flatMap(([,fields])=>fields);
 assert.equal(new Set(fields.map(field=>field.key)).size,fields.length);
 assert(fields.every(f=>f.format&&f.help&&f.example));
 assert.deepEqual(validateSettings(base),{});
 assert.deepEqual(validateSettings({...base,worker_bind:'[0:0:0:0:0:0:0:1]:4171',ssh_command:'/usr/bin/ssh',memory_budget_mib:512}),{});
});
test('loopback addresses, required fields, paths and admission limits fail before saving',()=>{
 for(const bind of ['localhost:4170','http://127.0.0.1:4170','0.0.0.0:4170','[::]:4170','127.0.0.1:0','127.0.0.1:65536','127.300.0.1:1','127.00.0.1:1'])assert(validateSettings({...base,bind}).bind,bind);
 assert(validateSettings({...base,bind:'[::1]:4170',worker_bind:'[0:0:0:0:0:0:0:1]:04170'}).worker_bind);
 for(const value of [0,33,1.5,Infinity])assert(validateSettings({...base,max_active_runs:value}).max_active_runs);
 assert(validateSettings({...base,default_stage_memory_mib:257,memory_budget_mib:256}).default_stage_memory_mib);
 assert(validateSettings({...base,default_stage_memory_mib:Number.MAX_SAFE_INTEGER+1}).default_stage_memory_mib);
 assert.deepEqual(validateSettings({...base,allowed_roots:[]}),{});
 assert(validateSettings({...base,data_dir:'~/data'}).data_dir);
 assert(validateSettings({...base,codex_command:'codex --foo'}).codex_command);
 assert.deepEqual(validateSettings({...base,data_dir:'/home/user/My Data',allowed_roots:['/home/user/My Projects']}),{});
});
test('completion delivery accepts only supported modes and requires desktop transport for immediate mode',()=>{
 assert.deepEqual(validateSettings({...base,codex_delivery_mode:'queued'}),{});
 assert(validateSettings({...base,codex_delivery_mode:'invalid'}).codex_delivery_mode);
 assert(validateSettings({...base,codex_delivery_mode:'immediate'}).codex_ipc_socket);
 assert(validateSettings({...base,codex_delivery_mode:'immediate',codex_ipc_socket:'/private/codex.sock'}).codex_command);
 assert.deepEqual(validateSettings({...base,codex_delivery_mode:'immediate',codex_command:'/usr/local/bin/codex',codex_ipc_socket:'/private/codex.sock'}),{});
 assert(validateSettings({...base,codex_command:'/usr/local/bin/codex',codex_ipc_socket:'relative.sock'}).codex_ipc_socket);
});
test('retention policies enforce names, project selection and bounded integer units',()=>{
 const p={category:'stage_workspaces',name:'weekly',project_id:'01a181b1-eacd-7661-8b99-a38b5ee6e4f6',ttl_seconds:604800,interval_seconds:3600,batch_limit:25};
 assert.deepEqual(validatePolicies([p]),[{}]);
 assert.deepEqual(validatePolicies([{...p,category:'artifact_expiry'}]),[{}]);
 assert(validatePolicies([{...p,category:'unknown'}])[0].category);
 assert(validatePolicies([p,p])[1].name);
 for(const key of ['ttl_seconds','interval_seconds','batch_limit'])for(const v of [null,0,1.5,Number.MAX_SAFE_INTEGER])assert(validatePolicies([{...p,[key]:v}])[0][key]);
 assert(validatePolicies([{...p,project_id:''}])[0].project_id);
 assert(validatePolicies([{...p,name:'invalid name'}])[0].name);
});

test('quota watermarks require a complete ordered pair of safe integer byte counts',()=>{
 const p={category:'stage_workspaces',name:'quota',project_id:'01a181b1-eacd-7661-8b99-a38b5ee6e4f6',ttl_seconds:60,interval_seconds:60,batch_limit:1};
 assert.deepEqual(validatePolicies([{...p,quota:{high_bytes:100,low_bytes:0}}]),[{}]);
 for(const quota of [{high_bytes:0,low_bytes:0},{high_bytes:100,low_bytes:100},{high_bytes:100,low_bytes:null},{high_bytes:Number.MAX_SAFE_INTEGER+1,low_bytes:0}])assert(Object.keys(validatePolicies([{...p,quota}])[0]).length);
});

test('disk trigger fields require positive ordered bytes and exclude a size quota',()=>{
 const p={category:'stage_workspaces',name:'disk',project_id:'01a181b1-eacd-7661-8b99-a38b5ee6e4f6',ttl_seconds:60,interval_seconds:60,batch_limit:1};
 assert.deepEqual(validatePolicies([{...p,disk_pressure:{start_below_bytes:100,stop_at_bytes:200}}]),[{}]);
 for(const disk_pressure of [{start_below_bytes:0,stop_at_bytes:100},{start_below_bytes:100,stop_at_bytes:100},{start_below_bytes:100,stop_at_bytes:null},{start_below_bytes:100,stop_at_bytes:Number.MAX_SAFE_INTEGER+1}])assert(Object.keys(validatePolicies([{...p,disk_pressure}])[0]).length);
 assert(validatePolicies([{...p,disk_pressure:{start_below_bytes:100,stop_at_bytes:200},quota:{high_bytes:100,low_bytes:0}}])[0].disk_start_bytes);
});

test('retention categories keep log filesystem claims honest',()=>{
 const p={name:'retention',project_id:'01a181b1-eacd-7661-8b99-a38b5ee6e4f6',ttl_seconds:60,interval_seconds:60,batch_limit:1};
 assert(validatePolicies([{...p,category:'run_archives',pipeline_id:'cache-scope'}])[0].category);
 for(const category of ['logs','source_snapshots','module_copies','cache_artifacts','run_archives'])assert.deepEqual(validatePolicies([{...p,category}]),[{}]);
 assert(validatePolicies([{...p,category:'logs',disk_pressure:{start_below_bytes:1,stop_at_bytes:2}}])[0].disk_start_bytes);
 assert.deepEqual(validatePolicies([{...p,category:'logs',quota:{high_bytes:2,low_bytes:0}}]),[{}]);
});


test('CI source settings select workspaces and keep credentials in environment names',async()=>{
 const {validateCISources}=await import('../src/ci-settings.mjs');
 const source={name:'actions',project_id:'01a181b1-eacd-7661-8b99-a38b5ee6e4f6',provider:'github',repository:'org/repo',token_env:'PREFLIGHT_CI_TOKEN',poll_seconds:300};
 assert.deepEqual(validateCISources([source]),[{}]);
 assert(validateCISources([source,source])[1].name);
 for(const patch of [{repository:'../repo'},{repository:'repo'},{project_id:''},{token_env:'credential-value'},{poll_seconds:1},{api_base:'http://example.com'},{api_base:'https://user:secret@example.com'}])assert(Object.keys(validateCISources([{...source,...patch}])[0]).length);
 assert.deepEqual(validateCISources([{...source,provider:'gitlab',repository:'group/subgroup/repo'}]),[{}]);
});
test('operator executable fields support Unix and Windows absolute paths',()=>{
 for(const path of ['/usr/local/bin/docker','C:\\Program Files\\Docker\\docker.exe','C:/Tools/docker.exe','\\\\server\\share\\docker.exe'])assert.deepEqual(validateSettings({...base,docker_command:path}),{},path);
 for(const path of ['docker','C:docker.exe'])assert(validateSettings({...base,docker_command:path}).docker_command,path);
});

test('settings dirty state ignores JSON object order but preserves real edits',()=>{
 const a={access_tokens:[{name:'a',role:'execute',token_env:'TOKEN_A'}]};
 assert(settingsEqual(a,{access_tokens:[{token_env:'TOKEN_A',name:'a',role:'execute'}]}));
 assert(!settingsEqual(a,{access_tokens:[{name:'a',role:'admin',token_env:'TOKEN_A'}]}));
 assert(!settingsEqual({value:null},{}));assert(!settingsEqual({values:[1,2]},{values:[2,1]}));
});

test('managed disk threshold accepts whole MiB and rejects invalid values',()=>{
 assert.deepEqual(validateSettings({...base,max_storage_mib:102400}),{});
 for(const value of [0,-1,1.5,8589934592,Infinity])assert(validateSettings({...base,max_storage_mib:value}).max_storage_mib);
});
