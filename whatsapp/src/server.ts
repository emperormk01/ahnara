
import { serve } from "@hono/node-server";
import { Hono } from "hono";
import { WhatsAppIntegration } from "./index";

const app = new Hono();
const wa = new WhatsAppIntegration({
  authDir: process.env.WHATSAPP_AUTH_DIR ?? "./auth_whatsapp",
});

const GATEWAY_URL = process.env.AHNARA_GATEWAY_URL ?? "http://localhost:18789";

// Initialize WhatsApp
await wa.connect();

// Forward incoming messages to Rust gateway
wa.onMessage(async (msg) => {
  const text = msg.message.conversation || msg.message.extendedTextMessage?.text || "";
  if (!text) return;

  try {
    await fetch(`${GATEWAY_URL}/api/whatsapp/message`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        jid: msg.key.remoteJid,
        pushName: msg.pushName,
        text: text,
      }),
    });
  } catch (e) {
    console.error("Failed to forward message to gateway:", e);
  }
});

// Endpoint to send messages from Rust gateway
app.post("/send", async (c) => {
  const { jid, text } = await c.req.json();
  try {
    await wa.sendMessage(jid, text);
    return c.json({ status: "ok" });
  } catch (e: any) {
    return c.json({ status: "error", message: e.message }, 500);
  }
});

// Endpoint to request pairing code
app.get("/pairing-code", async (c) => {
  const phone = c.req.query("phone");
  if (!phone) return c.json({ error: "Phone number required" }, 400);

  try {
    const code = await wa.requestPairingCode(phone);
    return c.json({ code });
  } catch (e: any) {
    return c.json({ error: e.message }, 500);
  }
});

// Endpoint for status checks
app.get("/status", async (c) => {
  return c.json({ connected: wa.isRegistered() });
});

console.log("WhatsApp Bridge listening on port 18790");
serve({
  fetch: app.fetch,
  port: 18790,
});
