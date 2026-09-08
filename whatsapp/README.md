# WhatsApp Integration

WhatsApp channel integration using Baileys library with **Pairing Code** flow (no QR scanning required).

## Features

- **Pairing Code Flow**: Link WhatsApp using a 8-digit code instead of QR scan
- **Multi-Device Support**: Works with your existing WhatsApp account
- **Persistent Sessions**: Auth state saved locally for reconnection
- **Message Handling**: Send/receive text messages
- **Event Callbacks**: Listen for messages and connection changes

## Installation

```bash
cd whatsapp
bun install
```

## Usage

### CLI Mode

```bash
# Provide your phone number when starting
bun run index.ts 2348030000000

# This will:
# 1. Connect to WhatsApp
# 2. Generate a pairing code
# 3. Display the code for you to enter in WhatsApp
```

### As a Library

```typescript
import { WhatsAppIntegration } from "./src/index";

const wa = new WhatsAppIntegration({
  authDir: "./auth_whatsapp",  // Directory for session storage
});

// Connect
await wa.connect();

// Request pairing code
if (!wa.isRegistered()) {
  const code = await wa.requestPairingCode("2348030000000");
  console.log(`Enter this code: ${code}`);
}

// Listen for connection changes
wa.onConnectionChange((state) => {
  console.log("Connection state:", state);
});

// Listen for messages
wa.onMessage((msg) => {
  console.log(`${msg.pushName}: ${msg.message.conversation}`);
});

// Send a message
await wa.sendMessage("2348030000000@s.whatsapp.net", "Hello from WhatsApp!");
```

## Pairing Flow

1. **User provides phone number** via the app
2. **App requests pairing code** from WhatsApp servers
3. **User enters code** in WhatsApp Settings > Linked Devices
4. **Connection established** - messages can now be sent/received

## API

### `WhatsAppIntegration`

#### `connect(): Promise<ConnectionState>`
Initialize the WhatsApp socket and load saved auth state.

#### `requestPairingCode(phoneNumber: string): Promise<string>`
Request an 8-digit pairing code for the given phone number.

#### `sendMessage(jid: string, text: string): Promise<any>`
Send a text message to the specified JID.

#### `onMessage(handler: (msg: WhatsAppMessage) => void): void`
Register a callback for incoming messages.

#### `onConnectionChange(handler: (state: ConnectionState) => void): void`
Register a callback for connection state changes.

#### `disconnect(): Promise<void>`
Disconnect from WhatsApp.

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `WHATSAPP_AUTH_DIR` | Directory for session storage | `./auth_whatsapp` |

## WhatsApp Bot Integration

To integrate with ahnara agent:

```typescript
import { WhatsAppIntegration } from "./src/index";
import { AgentCore } from "../agent";

const wa = new WhatsAppIntegration();
await wa.connect();

wa.onMessage(async (msg) => {
  const text = msg.message.conversation || msg.message.extendedTextMessage?.text;
  const response = await agent.process(text, msg.key.remoteJid);
  await wa.sendMessage(msg.key.remoteJid, response);
});
```