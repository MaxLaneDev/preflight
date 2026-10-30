import test from 'node:test';
import assert from 'node:assert/strict';
import {validateAccessTokens} from '../src/runtime-settings.mjs';
test('credential settings accept environment references and all three roles',()=>{
 assert.deepEqual(validateAccessTokens(['read_only','execute','admin'].map((role,n)=>({name:'user-'+n,token_env:'TOKEN_'+n,role}))),[{},{},{}]);
});
test('credential settings reject duplicate identities, invalid names and roles',()=>{
 const result=validateAccessTokens([{name:'a',token_env:'SECRET_A',role:'admin'},{name:'a',token_env:'SECRET_A',role:'owner'},{name:'a b',token_env:'actual-secret-value',role:'read_only'}]);assert(result[1].name&&result[1].token_env&&result[1].role);assert(result[2].name&&result[2].token_env);
});
