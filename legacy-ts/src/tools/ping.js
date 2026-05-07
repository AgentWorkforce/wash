import { meta } from '../burn/meta.js';

export const pingTool = {
  name: 'relaywash__Ping',
  description:
    'Health check / annotation pipeline probe. Returns "pong" plus a sample _meta field.',
  inputSchema: {
    type: 'object',
    properties: {},
    additionalProperties: false,
  },
  handler() {
    return { result: 'pong', _meta: meta([], 0) };
  },
};
