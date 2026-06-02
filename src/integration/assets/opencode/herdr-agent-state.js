// installed by herdr
// managed by herdr; reinstalling or updating the integration overwrites this file.
// add custom hooks/plugins beside this file instead of editing it.
// HERDR_INTEGRATION_ID=opencode
// HERDR_INTEGRATION_VERSION=4

import net from "node:net";

const SOURCE = "herdr:opencode";
let reportSeq = Date.now() * 1000;
let currentSessionTitle;

function nextReportSeq() {
  reportSeq += 1;
  return reportSeq;
}

function sessionIDFromProperties(properties) {
  return typeof properties?.sessionID === "string" && properties.sessionID
    ? properties.sessionID
    : undefined;
}

function sendRequest(request) {
  const paneId = process.env.HERDR_PANE_ID;
  const socketPath = process.env.HERDR_SOCKET_PATH;

  if (!paneId || !socketPath) {
    return Promise.resolve();
  }

  return new Promise((resolve) => {
    const client = net.createConnection(socketPath, () => {
      client.write(`${JSON.stringify(request)}\n`);
    });

    const finish = () => {
      client.destroy();
      resolve();
    };

    client.setTimeout(500, finish);
    client.on("data", finish);
    client.on("error", finish);
    client.on("end", finish);
    client.on("close", resolve);
  });
}

function reportSession(sessionID) {
  if (!sessionID) {
    return Promise.resolve();
  }
  const paneId = process.env.HERDR_PANE_ID;
  const socketPath = process.env.HERDR_SOCKET_PATH;

  if (!paneId || !socketPath) {
    return Promise.resolve();
  }

  const requestId = `${SOURCE}:${Date.now()}:${Math.floor(Math.random() * 1_000_000)
    .toString()
    .padStart(6, "0")}`;
  const request = {
    id: requestId,
    method: "pane.report_agent_session",
    params: {
      pane_id: paneId,
      source: SOURCE,
      agent: "opencode",
      seq: nextReportSeq(),
      agent_session_id: sessionID,
    },
  };

  return sendRequest(request);
}

function reportSessionTitle(title) {
  const paneId = process.env.HERDR_PANE_ID;
  const socketPath = process.env.HERDR_SOCKET_PATH;

  if (!paneId || !socketPath || !title) {
    return Promise.resolve();
  }

  const requestId = `${SOURCE}:meta:${Date.now()}:${Math.floor(Math.random() * 1_000_000)
    .toString()
    .padStart(6, "0")}`;
  const request = {
    id: requestId,
    method: "pane.report_metadata",
    params: {
      pane_id: paneId,
      source: SOURCE,
      agent: "opencode",
      applies_to_source: SOURCE,
      custom_status: title,
      seq: nextReportSeq(),
    },
  };

  return sendRequest(request);
}

export const HerdrAgentStatePlugin = async () => {
  if (
    process.env.HERDR_ENV !== "1" ||
    !process.env.HERDR_SOCKET_PATH ||
    !process.env.HERDR_PANE_ID
  ) {
    return {};
  }

  return {
    event: async ({ event }) => {
      const type = event?.type;
      const properties = event?.properties ?? {};
      const sessionID = sessionIDFromProperties(properties);

      switch (type) {
        case "session.created":
        case "session.updated": {
          // Extract the session title from the event info for display.
          const infoTitle = properties?.info?.title;
          if (typeof infoTitle === "string" && infoTitle.trim()) {
            currentSessionTitle = infoTitle.trim();
            await reportSessionTitle(currentSessionTitle);
          }
          await reportSession(sessionID);
          break;
        }
        case "session.status":
          await reportSession(sessionID);
          break;
        default:
          break;
      }
    },
  };
};
