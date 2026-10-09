/** Selected signed descriptors; host validation remains authoritative. */
import * as runtime from './schema-runtime.js';
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export interface SchemaType { kind: string; value?: unknown }
export interface ValidationIssue { path: {kind: 'field' | 'index' | 'mapKeyHash';value: string | number}[]; code: string; messageKey: string; message: string; parameters: Record<string,Json>; rule: string }
export interface ValidationReport { issues: ValidationIssue[]; truncated: boolean; work: number }
export interface SelectedSchema { format: 'axiom-validation-schema/v1'; models: Record<string,unknown>; enums: Record<string,string[]>; language: Record<string,unknown> }
export const validate = runtime.acoreSchemaValidate as (schema:SelectedSchema,type:SchemaType,value:Json,projection?:string[]|null)=>ValidationReport;
export const normalize = runtime.acoreSchemaNormalize as (schema:SelectedSchema,type:SchemaType,value:Json)=>Json;
export const codec = runtime.acoreSchemaCodec as (schema:SelectedSchema,name:string,value:Json,direction?:'encode'|'decode')=>Json;

export interface PublicFailureCase { model: string; payload: SchemaType; audiences: readonly string[]; status: number }
/** Literal selected descriptors infer a discriminated case and its payload. */
export type SchemaValue<T> = T extends {kind:'string'} ? string : T extends {kind:'int'|'float'} ? number : T extends {kind:'bool'} ? boolean : T extends {kind:'enum';value:readonly (infer V extends string)[]} ? V : T extends {kind:'list';value:infer V} ? SchemaValue<V>[] : T extends {kind:'optional';value:infer V} ? SchemaValue<V>|null : T extends {kind:'record';value:infer F} ? {[K in keyof F as F[K] extends {kind:'optional'} ? never : K]:SchemaValue<F[K]>}&{[K in keyof F as F[K] extends {kind:'optional'} ? K : never]?:SchemaValue<F[K]>} : Json;
export type FailureOutcome<C extends Record<string,PublicFailureCase>> = {[K in keyof C]:{kind:'failure';code:K;payload:SchemaValue<C[K]['payload']>}}[keyof C];
export const decodeFailure = runtime.acoreSchemaFailure as <C extends Record<string,PublicFailureCase>>(cases:C,wire:Json,status?:number|null)=>FailureOutcome<C>;
