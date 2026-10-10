import test from 'node:test';
import assert from 'node:assert/strict';
import {existsSync,readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {codec,validate,normalize} from '../dist/schema.js';
const schema={format:'axiom-validation-schema/v1',models:{Input:{count:{ty:{kind:'int32'},optional:false,rule:null,default:{value:4}}}},enums:{},language:{codecs:{id:{encoded:{kind:'string'},decoded:{kind:'int64'},decode:'parseCanonicalInt64',encode:'formatCanonicalInt64'}}}};
test('shared portable helper matches its pinned qualified source in standalone and workspace checkouts',()=>{
 const binding=JSON.parse(readFileSync(new URL('../schema-runtime.source.json',import.meta.url),'utf8'));
 assert.equal(binding.format,'axiom-portable-schema-source/v1');
 assert.equal(binding.sourcePath,'axiom-ui-host/web/acore-schema.js');
 const generated=readFileSync(new URL('../src/schema-runtime.ts',import.meta.url));
 const prefix=Buffer.from('// @ts-nocheck\n');
 assert.deepEqual(generated.subarray(0,prefix.length),prefix);
 const source=generated.subarray(prefix.length),sha=bytes=>createHash('sha256').update(bytes).digest('hex');
 assert.equal(sha(source),binding.sourceSha256);
 assert.equal(sha(generated),binding.generatedSha256);
 const host=new URL('../../../axiom-ui-host/web/acore-schema.js',import.meta.url);
 if(existsSync(host))assert.deepEqual(source,readFileSync(host));
});
test('selected validators and exact portable codecs reject malformed values',()=>{assert.equal(codec(schema,'id','123'),123);assert.throws(()=>codec(schema,'id','9007199254740993'));assert.deepEqual(JSON.parse(JSON.stringify(normalize(schema,{kind:'named',value:'Input'},{}))),{count:4});assert.equal(validate(schema,{kind:'named',value:'Input'},{count:null}).issues[0].code,'invalid_type');});

import {decodeFailure} from '../dist/schema.js';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const cases={titleTaken:{model:'Problem',status:409,audiences:['web'],payload:{kind:'record',value:{message:{kind:'string'}}}}};
test('selected failure helper checks payload, status, bounds and exact envelopes',()=>{
 assert.deepEqual(decodeFailure(cases,{error:{code:'titleTaken',payload:{message:'Choose another title'}}},409),{kind:'failure',code:'titleTaken',payload:{message:'Choose another title'}});
 for(const wire of [{error:{code:'unknown',payload:{message:'private'}}},{error:{code:'titleTaken',payload:{message:4}}},{error:{code:'titleTaken',payload:{message:'safe',privateReason:'PRIVATE'}}},{error:{code:'titleTaken',payload:{message:'x'.repeat(10001)}}},{error:{code:'titleTaken',payload:{message:'safe'}},extra:'PRIVATE'}])assert.throws(()=>decodeFailure(cases,wire,409));
 assert.throws(()=>decodeFailure(cases,{error:{code:'titleTaken',payload:{message:'safe'}}},404));
});
test('selected descriptors infer only public typed cases in a generated client',()=>{
 execFileSync(process.execPath,[fileURLToPath(new URL('../node_modules/typescript/bin/tsc',import.meta.url)),'--noEmit','--strict','--target','ES2022','--module','NodeNext','--moduleResolution','NodeNext',fileURLToPath(new URL('./schema-types.ts',import.meta.url))],{stdio:'pipe'});
});
