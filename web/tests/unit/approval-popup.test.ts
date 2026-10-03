import { expect, it } from 'vitest';
import { validApprovalMessage, manualApprovalCode, approvalUrl } from '../../src/lib/approval-popup';
const popup = {} as Window, origin = "https://app.example", state = "a".repeat(43);
const valid = { origin, source: popup, data: { type: "silicon:feature-approval", state, code: "obc_oneuse" } };
it('requires exact callback window, origin, state and one-use approval code', () => {
 expect(validApprovalMessage(valid,origin,popup,state)).toBe(true);
 for(const event of [{...valid,source:{} as Window},{...valid,origin:'https://evil.example'},{...valid,data:{...valid.data,state:'b'.repeat(43)}},{...valid,data:{...valid.data,code:'oba_access'}}]) expect(validApprovalMessage(event,origin,popup,state)).toBe(false);
});

it('manual recovery exposes only a valid single-use code from a bound callback', () => {
 const params = new URLSearchParams({state, code:"obc_manual"});
 expect(manualApprovalCode(params)).toBe("obc_manual");
 for (const bad of [new URLSearchParams({code:"obc_manual"}),new URLSearchParams({state,code:"oba_access"}),new URLSearchParams({state,code:"obc_manual",error:"access_denied"})]) expect(manualApprovalCode(bad)).toBeNull();
});

it('validates manual and popup review URLs under the same transport rules', () => {
 expect(approvalUrl("https://iam.example/review").href).toBe("https://iam.example/review");
 expect(approvalUrl("http://127.0.0.1/review").hostname).toBe("127.0.0.1");
 for(const bad of ["javascript:alert(1)","http://remote.example/review","https://user:secret@iam.example/review","https://iam.example/review#code"]) expect(()=>approvalUrl(bad)).toThrow();
});
