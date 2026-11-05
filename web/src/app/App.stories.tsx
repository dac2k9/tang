import type { Meta, StoryObj } from "@storybook/react-vite";
import { useMemo } from "react";

import { Client } from "../client/connection";
import { tracerSnapshot } from "../client/fixtures";
import { MockServer } from "../client/mock";
import type { MockOptions } from "../client/mock";
import { stressSnapshot } from "../client/stress";
import { App } from "./App";
import { ClientProvider } from "./ClientProvider";

/** The whole page over a mock engine with these options, which is what most of the interface will be built on. */
function OverMock({ server }: { server?: MockOptions }) {
  const client = useMemo(() => new Client({ transport: new MockServer(server).transport }), [server]);
  return (
    <ClientProvider client={client}>
      <App engine="mock" />
    </ClientProvider>
  );
}

const meta = {
  title: "App/Page",
  component: OverMock,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof OverMock>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The session of the mock, kept up to date. */
export const WithTheTracerSession: Story = {};

/** Eight lanes, 32 LFOs and 64 knobs that move: what the performance test runs on. */
export const TheStressScene: Story = { args: { server: { snapshot: stressSnapshot() } } };

/** The tracer, with plugins that have a window of their own, on a machine that can show it (step 112): the "UI" button is on the node of each, and "Open its own window" is in its menu. */
function withWindows() {
  const copy = structuredClone(tracerSnapshot);
  for (const node of copy.graph.nodes) {
    if ((node.kind === "instrument" || node.kind === "effect") && node.plugin) node.plugin.hasEditor = true;
  }
  return copy;
}

export const WithPluginsThatHaveTheirOwnWindow: Story = { args: { server: { snapshot: withWindows(), windows: true } } };

/** A server that speaks another protocol, which the page refuses with a reason. */
export const ServerOfAnotherProtocol: Story = { args: { server: { protocol: 2 } } };

/** A server that wants a token the page was not given. */
export const ServerThatWantsAToken: Story = { args: { server: { token: "secret" } } };
