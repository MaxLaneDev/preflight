import test from 'node:test';
import assert from 'node:assert/strict';
import {releaseSummary,releaseLink} from '../src/releases.mjs';
import {validateSettings} from '../src/settings.mjs';
const base={bind:'127.0.0.1:4170',data_dir:'data',allowed_roots:['examples'],max_active_runs:32,max_parallel_commands:2,default_stage_memory_mib:256};
test('release repository input matches the server boundary',()=>{
 for(const value of ['owner/repo','a.b/repo_2'])assert.deepEqual(validateSettings({...base,release_repository:value}),{});
 for(const value of ['../b','a/..','a/b/c','https://github.com/a/b','a/b?token=x','a/%2f'])assert(validateSettings({...base,release_repository:value}).release_repository);
});
test('release notifications have concise English states and safe source links',()=>{
 assert.match(releaseSummary({status:'disabled'}),/disabled/);
 assert.match(releaseSummary({status:'unchecked'}),/Waiting/);
 assert.equal(releaseSummary({status:'error',error:'HTTP 429'}),'HTTP 429');
 assert.equal(releaseSummary({status:'available',update_available:true,latest_version:'0.2.0'}),'Preflight 0.2.0 is available.');
 assert.equal(releaseLink('https://github.com/owner/repo/releases/tag/v0.2.0'),'https://github.com/owner/repo/releases/tag/v0.2.0');
 for(const url of ['javascript:alert(1)','https://evil.test/owner/repo/releases/tag/v1','https://token@github.com/owner/repo/releases/tag/v1','https://github.com/owner/repo/releases/tag/v1?token=x','https://github.com/a/b','https://github.com:443/a/b/releases/tag/v1#x'])assert.equal(releaseLink(url),null,url);
});
