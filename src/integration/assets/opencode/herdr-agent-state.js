import net from "node:net";

const SOURCE = "herdr:opencode";

function reportState(action) {
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
    method: action === "release" ? "pane.release_agent" : "pane.report_agent",
    params:
      action === "release"
        ? {
            pane_id: paneId,
            source: SOURCE,
            agent: "opencode",
          }
        : {
            pane_id: paneId,
            source: SOURCE,
            agent: "opencode",
            state: action,
          },
  };

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

      switch (type) {
        case "permission.asked":
        case "question.asked":
          await reportState("blocked");
          break;
        case "permission.replied": {
          const reply = properties.reply ?? properties.response;
          if (reply === "reject") {
            await reportState("idle");
          } else if (reply === "once" || reply === "always") {
            await reportState("working");
          }
          break;
        }
        case "question.replied":
          await reportState("working");
          break;
        case "question.rejected":
          await reportState("idle");
          break;
        case "session.status": {
          const status =
            typeof properties.status === "string"
              ? properties.status
              : properties.status?.type;
          if (status === "busy" || status === "retry") {
            await reportState("working");
          } else if (status === "idle") {
            await reportState("idle");
          }
          break;
        }
        case "session.idle":
          await reportState("idle");
          break;
        default:
          break;
      }
    },
  };
};
