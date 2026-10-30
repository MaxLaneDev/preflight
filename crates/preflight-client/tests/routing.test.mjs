import test from 'node:test';
import assert from 'node:assert/strict';
import {routePath,readRoute} from '../src/routing.mjs';
test('search test target retains project, run, stage and literal test name',()=>{
 const values={project:'project-id',section:'pipelines',pipeline:'definition:a/b',run:'run-id',tab:'Tests',stage:'unit & integration',test:'Parser::test + #1'};
 const route=readRoute(new URL(routePath(values),'http://127.0.0.1'));
 for(const key of Object.keys(values))assert.equal(route.get(key),values[key]);
 assert(!route.has('error'));
});

test('external CI section has a project-scoped deep link',()=>{
 const path=routePath({project:'workspace',section:'ci'});
 assert.equal(path,'/projects/workspace/ci');
 const route=readRoute(new URL(path,'http://localhost'));
 assert.equal(route.get('project'),'workspace');assert.equal(route.get('section'),'ci');
});
