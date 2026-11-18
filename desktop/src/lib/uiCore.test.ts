import { expect, it } from 'vitest';
import * as ui from './uiCore';
it('exports pure helpers and public Help without native authority',()=>{
  expect(ui.queueKind('/help')).toBe('command');
  expect(ui.nextAssistantText('a','a')).toBe('aa');
  expect(ui.unclosedFenceStart('hello')).toBe(-1);
  for (const name of ['invoke','helpAsk','helpFollow','helpCancel','createHelpAuthority']) expect(name in ui).toBe(false);
});
