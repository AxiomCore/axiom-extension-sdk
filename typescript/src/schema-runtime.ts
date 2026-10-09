// @ts-nocheck
// axiom-validation-schema/v1. Host-free portable validation for selected,
// signed descriptors. This is also the generated TypeScript guest helper.
export const ACORE_SCHEMA_FORMAT = 'axiom-validation-schema/v1';
const own = (o,k) => Object.prototype.hasOwnProperty.call(o,k);
const record = v => v !== null && typeof v === 'object' && !Array.isArray(v);
const bytes = s => {let n=0;for(const c of s){const p=c.codePointAt(0);if(p>=0xd800&&p<=0xdfff)throw Error('invalid Unicode');n+=p<128?1:p<2048?2:p<65536?3:4;}return n;};
const order = (a,b) => { const x=[...a],y=[...b];for(let i=0;i<Math.min(x.length,y.length);i++){const d=x[i].codePointAt(0)-y[i].codePointAt(0);if(d)return d;}return x.length-y.length; };
const canonical = v => Array.isArray(v)?'['+v.map(canonical).join(',')+']':record(v)?'{'+Object.keys(v).sort(order).map(k=>JSON.stringify(k)+':'+canonical(v[k])).join(',')+'}':JSON.stringify(v);
function state(){return {fuel:10000,size:0,issues:[],truncated:false};}
function spend(s,n=1){if(n>s.fuel){s.fuel=0;throw Error('work limit');}s.fuel-=n;}
function issue(s,path,code,message,rule='structural'){
 const value={path:[...path],code,messageKey:'validation.'+code,message,parameters:{},rule},size=bytes(JSON.stringify(value));
 if(s.issues.length>=64||s.size+size+128>32768){s.truncated=true;return;}s.size+=size;s.issues.push(value);
}
function bound(value,s,depth=0){spend(s);if(depth>32)throw Error('value depth');if(typeof value==='string')bytes(value);else if(Array.isArray(value))for(const v of value)bound(v,s,depth+1);else if(record(value))for(const [k,v]of Object.entries(value)){bytes(k);bound(v,s,depth+1);}else if(value!==null&&typeof value!=='boolean'&&(typeof value!=='number'||!Number.isFinite(value)))throw Error('invalid JSON');}
const trim = v => v.replace(/^[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+|[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+$/gu,'');
function defaultValue(f){if(!own(f.default,'value')||Object.keys(f.default).length!==1)throw Error('default requires an exact target representation');return f.default.value;}
function fields(e,out=new Set()){
 if(e.kind==='name'&&e.name==='Input')out.add('*');
 if(e.kind==='field'&&e.target.kind==='name'&&e.target.name==='Input')out.add(e.name);
 else for(const [k,v]of Object.entries(e)){if(k==='kind')continue;if(Array.isArray(v)){for(const x of v){if(Array.isArray(x))fields(x[1],out);else if(x&&typeof x==='object')fields(x,out);}}else if(v&&typeof v==='object')fields(v,out);}
 return out;
}
function type(t){const k=t.kind.toLowerCase();return {...t,kind:['int32','int64'].includes(k)?'int':['float32','float64'].includes(k)?'float':['bytes','datetime'].includes(k)?'string':k,...(k==='map'&&Array.isArray(t.value)?{value:type(t.value[1])}:{})};}
function fieldType(t,name,schema){t=type(t);if(t.kind==='optional')t=type(t.value);if(t.kind==='named'){const fs=own(schema.models,t.value)?schema.models[t.value]:null;const f=fs&&own(fs,name)?fs[name]:null;if(!f)throw Error('unknown field');return (f.optional&&!own(f,'default')||nullableRule(f.rule))?{kind:'optional',value:type(f.ty)}:type(f.ty);}if(t.kind==='record')return type(t.value[name]);if(name==='length'&&(t.kind==='string'||t.kind==='list'))return {kind:'int'};throw Error('invalid field');}
function checked(v,t){if(typeof v==='number'&&(!Number.isFinite(v)||t.kind==='int'&&!Number.isSafeInteger(v)))throw Error('numeric overflow');return {v,t};}
function pure(e,env,schema,s,depth=0){spend(s);if(depth>32)throw Error('pure depth');const run=x=>pure(x,env,schema,s,depth+1),out=(v,t)=>{bound(v,state());spend(s,bytes(JSON.stringify(v)));return checked(v,t);};
 switch(e.kind){
 case 'literal':return out(e.value,{kind:e.value===null?'null':typeof e.value==='number'?'int':typeof e.value==='boolean'?'bool':typeof e.value});
 case 'numeric':{const v=Number(e.representation);if(e.integer&&(!Number.isSafeInteger(v)||String(v)!==e.representation))throw Error('nonportable integer');return out(v,{kind:e.integer?'int':'float'});}
 case 'name':if(!own(env,e.name))throw Error('unknown pure name');return env[e.name];
 case 'field':{const a=run(e.target),t=fieldType(a.t,e.name,schema);if(a.v==null&&e.optional)return out(null,{kind:'optional',value:t});if(e.name==='length'&&(typeof a.v==='string'||Array.isArray(a.v))){if(typeof a.v==='string')spend(s,bytes(a.v));return out(typeof a.v==='string'?[...a.v].length:a.v.length,{kind:'int'});}return out(own(a.v,e.name)?a.v[e.name]:null,t);}
 case 'index':{const a=run(e.target),b=run(e.index);return out(own(a.v,b.v)?a.v[b.v]:null,{kind:'optional',value:a.t.value||{kind:'json'}});}
 case 'record':{const v=Object.create(null),t=Object.create(null);for(const [n,x]of e.fields){const a=run(x);Object.defineProperty(v,n,{value:a.v,enumerable:true});t[n]=a.t;}return out(v,{kind:'record',value:t});}
 case 'list':{const a=e.items.map(run);return out(a.map(x=>x.v),{kind:'list',value:a[0]?.t||{kind:'json'}});}
 case 'unary':{const a=run(e.value);return out(e.operator==='!'?!a.v:-a.v,e.operator==='!'?{kind:'bool'}:a.t);}
 case 'conditional':return run(run(e.condition).v?e.yes:e.no);
 case 'binary':{const a=run(e.left),op=e.operator;if(op==='&&'&&!a.v)return out(false,{kind:'bool'});if(op==='||'&&a.v)return out(true,{kind:'bool'});if(op==='??'&&a.v!=null)return out(a.v,a.t.value||a.t);const b=run(e.right);if(op==='??')return b;let v;
  switch(op){case '&&':case '||':v=b.v;break;case '==':v=canonical(a.v)===canonical(b.v);break;case '!=':v=canonical(a.v)!==canonical(b.v);break;case '>':v=a.v>b.v;break;case '>=':v=a.v>=b.v;break;case '<':v=a.v<b.v;break;case '<=':v=a.v<=b.v;break;case '+':if(typeof a.v==='string')spend(s,bytes(a.v)+bytes(b.v));v=a.v+b.v;break;case '-':v=a.v-b.v;break;case '*':v=a.v*b.v;break;case '/':if(b.v===0)throw Error('division by zero');v=a.t.kind==='int'?Math.trunc(a.v/b.v):a.v/b.v;break;case '%':if(b.v===0)throw Error('division by zero');v=a.v%b.v;break;default:throw Error('unknown operator');}
  return out(v,typeof v==='boolean'?{kind:'bool'}:a.t);}
 case 'call':{const name=e.function.replace(/^Functions\./,''),args=e.arguments.map(run),all=schema.language.functions||{},f=own(all,name)?all[name]:null;if(f){if(args.length!==f.parameters.length)throw Error('argument count');const scope=Object.create(null);f.parameters.forEach(([n,t],i)=>{scope[n]=out(args[i].v,type(t));});return out(pure(f.body,scope,schema,s,depth+1).v,type(f.result));}return builtin(name,args,s);}
 case 'method':{const a=run(e.target),args=e.arguments.map(run).map(x=>x.v),v=a.v;spend(s,typeof v==='string'?bytes(v):v.length);let r;
  switch(e.method){case 'trim':r=trim(v);break;case 'toLowerCase':r=v.toLowerCase();break;case 'toUpperCase':r=v.toUpperCase();break;case 'startsWith':r=v.startsWith(args[0]);break;case 'endsWith':r=v.endsWith(args[0]);break;case 'contains':r=typeof v==='string'?v.includes(args[0]):v.some(x=>canonical(x)===canonical(args[0]));break;case 'slice':if(args[0]<0||args[1]<args[0])throw Error('slice bounds');r=[...v].slice(args[0],args[1]).join('');break;case 'take':case 'drop':if(args[0]<0)throw Error('collection bound');r=e.method==='take'?v.slice(0,args[0]):v.slice(args[0]);break;case 'sum':r=0;for(const n of v){r+=n;if(!Number.isSafeInteger(r))throw Error('integer overflow');}break;case 'join':{const size=v.reduce((n,x)=>n+bytes(x),bytes(args[0])*Math.max(0,v.length-1));if(size>1048576)throw Error('join byte limit');spend(s,size);r=v.join(args[0]);break;}default:throw Error('unsupported method');}
  return out(r,typeof r==='boolean'?{kind:'bool'}:typeof r==='number'?{kind:'int'}:typeof r==='string'?{kind:'string'}:a.t);}
 default:throw Error('unknown pure expression');
 }
}
function date(text){if(!/^\d{4}-\d{2}-\d{2}$/.test(text))return false;const [y,m,d]=text.split('-').map(Number),days=[31,(y%4===0&&(y%100!==0||y%400===0))?29:28,31,30,31,30,31,31,30,31,30,31];return m>=1&&m<=12&&d>=1&&d<=days[m-1];}
function timestamp(text){const m=/^(\d{4}-\d{2}-\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d{3}|\d{6}|\d{9}))?Z$/.exec(text);return !!m&&date(m[1])&&+m[2]<24&&+m[3]<60&&+m[4]<60&&(!m[5]||m[5].slice(-3)!=='000');}
function base64(text){if(!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(text))return false;const chars='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';return text.endsWith('==')?chars.indexOf(text[text.length-3])%16===0:text.endsWith('=')?chars.indexOf(text[text.length-2])%4===0:true;}
function builtin(name,args,s){if(args.length!==1)throw Error('conversion argument count');const v=args[0].v;if(name==='formatCanonicalInt64'){if(!Number.isSafeInteger(v))throw Error('nonportable integer');return {v:String(v),t:{kind:'string'}};}if(typeof v!=='string')throw Error('expected string');spend(s,bytes(v));let valid;
 switch(name){case 'parseCanonicalInt64':{const n=Number(v);if(!Number.isSafeInteger(n)||String(n)!==v)throw Error('noncanonical or nonportable integer');return {v:n,t:{kind:'int'}};}case 'canonicalBigInt':valid=v.length<=128&&/^(?:0|-?[1-9][0-9]*)$/.test(v);break;case 'canonicalDecimal':valid=v.length<=128&&/^-?(?:0|[1-9][0-9]*)\.(?:0|[0-9]*[1-9])$/.test(v)&&v!=='-0.0';break;case 'canonicalBase64':valid=base64(v);break;case 'canonicalDate':valid=date(v);break;case 'canonicalTimestamp':valid=timestamp(v);break;default:throw Error('unknown intrinsic');}
 if(!valid)throw Error('invalid canonical representation');return {v,t:{kind:'string'}};
}
function rule(r,v,s,depth=0,defs={}){spend(s);if(depth>32)throw Error('rule depth');switch(r.type){
 case 'optional':return v===undefined||v===null||rule(r.inner,v,s,depth+1,defs);case 'nullable':return v===null||rule(r.inner,v,s,depth+1,defs);
 case 'recursive':case 'ref':throw Error('unqualified recursive rule');
 case 'any':return true;case 'never':return false;case 'boolean':return typeof v==='boolean';case 'literal':return canonical(v)===canonical(r.value);case 'enum':return r.values.some(x=>canonical(v)===canonical(x));
 case 'number':return typeof v==='number'&&Number.isFinite(v)&&(!r.int||Number.isSafeInteger(v))&&(r.min==null||v>=r.min)&&(r.max==null||v<=r.max);
 case 'string':{if(typeof v!=='string')return false;spend(s,bytes(v));const n=[...v].length;if(r.trim)v=trim(v);
  if(r.min!=null&&n<r.min||r.max!=null&&n>r.max||r.length!=null&&n!==r.length||r.starts_with!=null&&!v.startsWith(r.starts_with)||r.ends_with!=null&&!v.endsWith(r.ends_with)||r.includes!=null&&!v.includes(r.includes))return false;
  if(r.regex){if(!portableRegex(r.regex))throw Error('unqualified portable regex');if(!new RegExp(portablePattern(r.regex),'u').test(v))return false;}
  if(r.uuid&&!/^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/.test(v))return false;
  if(r.cuid&&!(v.startsWith('c')&&v.length>=8))return false;
  if(r.datetime&&!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})?$/.test(v))return false;
  if(r.email&&!/^[a-zA-Z0-9_.+-]+@[a-zA-Z0-9-]+\.[a-zA-Z0-9-.]+$/.test(v))return false;
  if(r.url&&!(v.startsWith('http://')||v.startsWith('https://')))return false;if(r.ip||r.cuid2||r.ulid)throw Error('unqualified portable format');return true;}
 case 'union':return r.options.some(x=>rule(x,v,s,depth+1,defs));
 case 'array':return Array.isArray(v)&&(r.min==null||v.length>=r.min)&&(r.max==null||v.length<=r.max)&&v.every(x=>rule(r.items,x,s,depth+1,defs));
 case 'tuple':return Array.isArray(v)&&v.length===r.items.length&&v.every((x,i)=>rule(r.items[i],x,s,depth+1,defs));
 case 'object':return record(v)&&(!r.strict||Object.keys(v).every(k=>own(r.properties,k)))&&Object.entries(r.properties).every(([k,t])=>rule(t,own(v,k)?v[k]:undefined,s,depth+1,defs));
 case 'record':case 'map':return record(v)&&Object.entries(v).every(([k,x])=>rule(r.key,k,s,depth+1,defs)&&rule(r.value,x,s,depth+1,defs));
 case 'set':return Array.isArray(v)&&(r.min==null||v.length>=r.min)&&new Set(v.map(canonical)).size===v.length&&v.every(x=>rule(r.value,x,s,depth+1,defs));
 case 'discriminatedUnion':return record(v)&&r.options.some(x=>rule(x,v,s,depth+1,defs));
 case 'date':throw Error('unqualified portable date rule');default:throw Error('unknown rule');}}
export function acoreSchemaValidate(schema,ty,value,projection=null){
 const s=state();if(schema?.format!==ACORE_SCHEMA_FORMAT)throw Error('unsupported validation schema');
 const walk=(t,v,path,allowed=null,depth=0)=>{if(s.truncated)return;spend(s);if(depth>32){s.truncated=true;issue(s,path,'depth_limit','Value exceeds the nesting limit');return;}let valid=true;
  switch(t.kind){case 'string':valid=typeof v==='string'&&[...v].length<=10000;break;case 'bool':valid=typeof v==='boolean';break;case 'void':valid=v===null;break;case 'json':valid=portableJson(v);break;case 'int32':valid=Number.isInteger(v)&&v>=-2147483648&&v<=2147483647;break;case 'int64':valid=Number.isSafeInteger(v);break;case 'float32':valid=typeof v==='number'&&Number.isFinite(Math.fround(v))&&(!Number.isInteger(v)||Number.isSafeInteger(v));break;case 'float64':valid=typeof v==='number'&&Number.isFinite(v)&&(!Number.isInteger(v)||Number.isSafeInteger(v));break;case 'bytes':case 'dateTime':if(typeof v!=='string'){valid=false;break;}try{spend(s,bytes(v));}catch(_){s.truncated=true;issue(s,path,'work_limit','Validation work limit reached');return;}valid=t.kind==='bytes'?base64(v):timestamp(v);break;
  case 'list':valid=Array.isArray(v);if(valid)v.forEach((x,i)=>walk(t.value,x,[...path,{kind:'index',value:i}],null,depth+1));break;
  case 'map':valid=record(v)&&t.value[0].kind==='string';if(valid)for(const k of Object.keys(v).sort(order))walk(t.value[1],v[k],[...path,{kind:'mapKeyHash',value:sha256(k).slice(0,16)}],null,depth+1);break;
  case 'named':{if(own(schema.enums,t.value)){valid=schema.enums[t.value].includes(v);break;}const fs=own(schema.models,t.value)?schema.models[t.value]:null;valid=!!fs&&record(v);if(!valid)break;const before=s.issues.length;
   if(Object.keys(v).some(k=>!own(fs,k)||allowed&&!allowed.includes(k)))issue(s,path,'unknown_field','Object contains an undeclared field');
   const input={...v};for(const k of Object.keys(fs).sort(order)){if(allowed&&!allowed.includes(k))continue;spend(s);const f=fs[k],p=[...path,{kind:'field',value:k}],present=own(v,k)||own(f,"default");
    if(!present){if(!f.optional)issue(s,p,'missing_field','Required field is missing',t.value+'.'+k);continue;}
    const x=own(v,k)?v[k]:defaultValue(f);input[k]=x;const start=s.issues.length,nullable=x===null&&nullableRule(f.rule);if(!nullable)walk(f.ty,x,p,null,depth+1);
    if(s.issues.length===start&&!s.truncated&&f.rule){try{if(!rule(f.rule,x,s))issue(s,p,'constraint','Value violates its declared constraints',t.value+'.'+k);}catch(_){s.truncated=true;issue(s,p,'work_limit','Validation work limit reached');}}
   }
   for(const name of (schema.language.validations||{})[t.value]||[]){const validator=(schema.language.validators||{})[name];for(const c of validator.checks){spend(s);const deps=fields(c.condition);if(allowed&&([...deps].some(f=>!allowed.includes(f))||c.field[0]&&!allowed.includes(c.field[0])))continue;
    if(s.issues.slice(before).filter(i=>['missing_field','invalid_type','depth_limit'].includes(i.code)).some(i=>deps.has('*')||i.path[path.length]?.kind==='field'&&deps.has(i.path[path.length].value)))continue;
    try{const result=pure(c.condition,{Input:{v:input,t:{kind:'named',value:t.value}}},schema,s).v;if(result===false)issue(s,[...path,...c.field.map(value=>({kind:'field',value}))],c.code,c.message,name);else if(result!==true)throw Error('validator result');}catch(_){s.truncated=true;issue(s,path,'validator_fault','Validator could not complete within its checked bounds',name);return;}
   }}break;}
  default:throw Error('unknown schema type');}
  if(!valid)issue(s,path,'invalid_type','Value has the wrong type or representation');
 };
 try{bound(value,state());if(bytes(JSON.stringify(value))>1048576)throw Error('value bytes');}catch(_){s.truncated=true;issue(s,[],'value_limit','Value exceeds the byte limit');}
 if(!s.truncated)try{walk(ty,value,[],projection);}catch(error){s.truncated=true;issue(s,[],error.message==='work limit'?'work_limit':'validator_fault',error.message==='work limit'?'Validation work limit reached':'Validator could not complete within its checked bounds');}
 return {issues:s.issues,truncated:s.truncated,work:10000-s.fuel};
}
function nullableRule(r){if(!r)return false;switch(r.type){case 'nullable':case 'any':return true;case 'optional':return nullableRule(r.inner);case 'literal':return r.value===null;case 'union':return r.options.some(nullableRule);default:return false;}}
export function acoreSchemaNormalize(schema,ty,value){
 const report=acoreSchemaValidate(schema,ty,value);if(report.truncated||report.issues.length){const e=Error('Input violates its checked schema');e.kind='validation';e.validation=report;throw e;}
 const s=state();function go(t,v,depth=0){spend(s);if(depth>32)throw Error('normalization depth');if(t.kind==='named'&&record(v)&&own(schema.models,t.value)){const out=Object.create(null);for(const [k,x]of Object.entries(v))out[k]=x;for(const [k,f]of Object.entries(schema.models[t.value])){if(!own(out,k)&&own(f,'default'))out[k]=defaultValue(f);if(own(out,k))out[k]=go(f.ty,out[k],depth+1);}return out;}if(t.kind==='list')return v.map(x=>go(t.value,x,depth+1));if(t.kind==='map'){const out=Object.create(null);for(const [k,x]of Object.entries(v))out[k]=go(t.value[1],x,depth+1);return out;}return v;}
 const out=go(ty,value);bound(out,s);if(bytes(JSON.stringify(out))>1048576)throw Error('normalization bytes');return out;
}
export function acoreSchemaCodec(schema,name,value,direction='decode'){
 const c=(schema.language.codecs||{})[name];if(!c||!['decode','encode'].includes(direction))throw Error('unknown codec');
 const input=direction==='decode'?c.encoded:c.decoded,output=direction==='decode'?c.decoded:c.encoded,s=state();
 if(!validReport(acoreSchemaValidate(schema,input,value)))throw Error('invalid codec input');
 const convert=(fn,v,t)=>pure({kind:'call',function:fn,arguments:[{kind:'name',name:'value'}]},{value:{v,t:type(t)}},schema,s).v;
 const out=convert(c[direction],value,input);if(!validReport(acoreSchemaValidate(schema,output,out)))throw Error('invalid codec output');
 if(canonical(convert(c[direction==='decode'?'encode':'decode'],out,output))!==canonical(value))throw Error('noncanonical or lossy codec');return out;
}
function validReport(r){return !r.truncated&&!r.issues.length;}
export function acoreSchemaRequest(descriptor,args){if(!descriptor)return args;const report=acoreSchemaValidate(descriptor.schema,{kind:'named',value:descriptor.requestModel},args);if(report.truncated||report.issues.length){const error=Error('Input violates its checked schema');error.kind='validation';error.validation=report;throw error;}return acoreSchemaNormalize(descriptor.schema,{kind:'named',value:descriptor.requestModel},args);}

// A single unprojected body parameter binds the whole JSON body. Projected
// records already expose their public fields directly; several body parameters
// use a field envelope. Path, query and header arguments are removed first.
export function acoreSchemaBody(operation,args){
 const request=operation.schema?.request;
 if(!request||operation.requestProjection)return args;
 const fields=Object.keys(request).filter(name=>request[name].source==='body');
 return fields.length===1?args[fields[0]]:args;
}

// SHA-256 is used only for non-reversible issue-path key identifiers.
function sha256(text){
 const data=[];for(const ch of text){const c=ch.codePointAt(0);if(c<128)data.push(c);else if(c<2048)data.push(192|(c>>6),128|(c&63));else if(c<65536)data.push(224|(c>>12),128|((c>>6)&63),128|(c&63));else data.push(240|(c>>18),128|((c>>12)&63),128|((c>>6)&63),128|(c&63));}
 const size=data.length;data.push(128);while(data.length%64!==56)data.push(0);for(let i=7;i>=0;i--)data.push(Math.floor(size*8/2**(i*8))&255);
 const k=[0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2];
 const h=[0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19],r=(v,n)=>(v>>>n)|(v<<(32-n));
 for(let off=0;off<data.length;off+=64){const w=[];for(let i=0;i<16;i++)w[i]=(data[off+i*4]<<24)|(data[off+i*4+1]<<16)|(data[off+i*4+2]<<8)|data[off+i*4+3];for(let i=16;i<64;i++){const a=w[i-15],b=w[i-2];w[i]=(w[i-16]+(r(a,7)^r(a,18)^(a>>>3))+w[i-7]+(r(b,17)^r(b,19)^(b>>>10)))|0;}
 let [a,b,c,d,e,f,g,j]=h;for(let i=0;i<64;i++){const t=(j+(r(e,6)^r(e,11)^r(e,25))+((e&f)^(~e&g))+k[i]+w[i])|0,u=((r(a,2)^r(a,13)^r(a,22))+((a&b)^(a&c)^(b&c)))|0;j=g;g=f;f=e;e=(d+t)|0;d=c;c=b;b=a;a=(t+u)|0;}const v=[a,b,c,d,e,f,g,j];for(let i=0;i<8;i++)h[i]=(h[i]+v[i])|0;}
 return h.map(x=>(x>>>0).toString(16).padStart(8,'0')).join('');
}

function portableRegex(pattern){if(pattern.length>256)return false;let escaped=false,cls=false,repeats=0;for(const c of pattern){if(escaped){if(!['n','r','t','f','\\','.','+','*','?','[',']','^','$','-'].includes(c))return false;escaped=false;continue;}if(c==='\\'){escaped=true;continue;}if(c==='[')cls=true;else if(c===']')cls=false;else if('(){}|'.includes(c))return false;else if(!cls&&'*+?'.includes(c))repeats++;}return !escaped&&!cls&&repeats<=1;}
function portablePattern(pattern){let out='',escaped=false,cls=false;for(const c of pattern){if(escaped){out+='\\'+c;escaped=false;continue;}if(c==='\\'){escaped=true;continue;}if(c==='[')cls=true;else if(c===']')cls=false;out+=!cls&&c==='.'?'[^\\n]':!cls&&c==='$'?'(?![\\s\\S])':c;}return out;}

function portableJson(v){const stack=[v];while(stack.length){const x=stack.pop();if(typeof x==='number'&&Number.isInteger(x)&&!Number.isSafeInteger(x))return false;if(Array.isArray(x))stack.push(...x);else if(record(x))stack.push(...Object.values(x));}return true;}

// The caller supplies the parsed transport envelope, never an arbitrary message.
export function acoreSchemaFailure(cases,wire,status=null){
 const s=state();bound(wire,s);if(bytes(JSON.stringify(wire))>65536)throw Error('failure envelope limit');
 if(!record(wire)||Object.keys(wire).length!==1||!own(wire,'error')||!record(wire.error)||Object.keys(wire.error).length!==2||!own(wire.error,'code')||!own(wire.error,'payload'))throw Error('malformed failure envelope');
 const {code,payload}=wire.error;if(typeof code!=='string'||!own(cases,code))throw Error('unknown failure case');const c=cases[code];if(status!==null&&status!==c.status)throw Error('failure status mismatch');
 function conforms(t,v,absent=false,depth=0){spend(s);if(depth>32)throw Error('failure type depth');switch(t.kind){case 'optional':return absent||v===null||conforms(t.value,v,false,depth+1);case 'string':return typeof v==='string'&&[...v].length<=10000;case 'int':return Number.isSafeInteger(v);case 'float':return typeof v==='number'&&Number.isFinite(v)&&(!Number.isInteger(v)||Number.isSafeInteger(v));case 'bool':return typeof v==='boolean';case 'enum':return t.value.includes(v);case 'list':return Array.isArray(v)&&v.every(x=>conforms(t.value,x,false,depth+1));case 'record':return record(v)&&Object.keys(v).every(k=>own(t.value,k))&&Object.entries(t.value).every(([k,t])=>conforms(t,v[k],!own(v,k),depth+1));default:return false;}}
 if(!conforms(c.payload,payload))throw Error('malformed typed failure payload');return {kind:'failure',code,payload};
}

/** Validate safe accumulated transport issues independently of business cases. */
export function acoreSchemaReport(report) {
 const s=state();bound(report,s);if(bytes(JSON.stringify(report))>32768||!record(report)||Object.keys(report).some(k=>!['issues','truncated','work'].includes(k))||!Array.isArray(report.issues)||report.issues.length>64||typeof report.truncated!=='boolean'||!Number.isSafeInteger(report.work)||report.work<0||report.work>10000)throw Error('malformed validation report');
 for(const issue of report.issues){
  if(!record(issue)||Object.keys(issue).sort().join(',')!=='code,message,messageKey,parameters,path,rule'||!Array.isArray(issue.path)||issue.path.length>32||typeof issue.code!=='string'||!/^\w{1,64}$/.test(issue.code)||typeof issue.messageKey!=='string'||bytes(issue.messageKey)>256||typeof issue.message!=='string'||bytes(issue.message)>256||typeof issue.rule!=='string'||bytes(issue.rule)>1024||!record(issue.parameters)||Object.keys(issue.parameters).length>16)throw Error('malformed validation issue');
  for(const part of issue.path)if(!record(part)||Object.keys(part).sort().join(',')!=='kind,value'||!(part.kind==='field'&&typeof part.value==='string'&&bytes(part.value)<=128||part.kind==='index'&&Number.isSafeInteger(part.value)&&part.value>=0||part.kind==='mapKeyHash'&&typeof part.value==='string'&&/^[a-f0-9]{16}$/.test(part.value)))throw Error('malformed validation path');
  for(const [key,value]of Object.entries(issue.parameters))if(!/^\w{1,64}$/.test(key)||!(value===null||typeof value==='boolean'||typeof value==='number'&&Number.isFinite(value)&&(!Number.isInteger(value)||Number.isSafeInteger(value))||typeof value==='string'&&bytes(value)<=256))throw Error('malformed validation parameters');
 }
 return report;
}
