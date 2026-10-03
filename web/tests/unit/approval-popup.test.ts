import { expect, it } from 'vitest';
import { validApprovalMessage } from '../../src/lib/approval-popup';
const popup = {} as Window, origin = "https://app.example", state = "a".repeat(43);
const valid = { origin, source: popup, data: { type: "silicon:feature-approval", state, code: "obc_oneuse" } };
it('requires exact callback window, origin, state and one-use approval code', () => {
 expect(validApprovalMessage(valid,origin,popup,state)).toBe(true);
 for(const event of [{...valid,source:{} as Window},{...valid,origin:'https://evil.example'},{...valid,data:{...valid.data,state:'b'.repeat(43)}},{...valid,data:{...valid.data,code:'oba_access'}}]) expect(validApprovalMessage(event,origin,popup,state)).toBe(false);
});
