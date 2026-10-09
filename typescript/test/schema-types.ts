import {decodeFailure, type FailureOutcome} from '../dist/schema.js';
const selected = {
 titleTaken: {model:'Problem',status:409,audiences:['web'],payload:{kind:'record',value:{message:{kind:'string'}}}},
 missing: {model:'Missing',status:404,audiences:['web'],payload:{kind:'record',value:{id:{kind:'int'},reason:{kind:'enum',value:['removed','hidden']}}}},
} as const;
type Cases = FailureOutcome<typeof selected>;
const result:Cases=decodeFailure(selected,{error:{code:'titleTaken',payload:{message:'Use another title'}}},409);
if(result.code==='titleTaken') {
 const message:string=result.payload.message;
 // @ts-expect-error private payload fields are not a selected client surface
 result.payload.privateReason;
 // @ts-expect-error a different case cannot be accessed without narrowing
 result.payload.id;
 void message;
} else {
 const reason:'removed'|'hidden'=result.payload.reason;
 const id:number=result.payload.id;
 void reason;void id;
}
// @ts-expect-error unselected business codes do not belong to the outcome union
const unknown:Cases={kind:'failure',code:'privateCase',payload:{message:'private'}};
void unknown;
