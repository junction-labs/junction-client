import { Client } from "./core.cjs";
import type { ClientOpts } from "./core.cjs";

import { Agent } from "./undici.cjs";
import type { AgentOpts } from "./undici.cjs";

export { Client, Agent };
export type { ClientOpts, AgentOpts as AgentOptions };
