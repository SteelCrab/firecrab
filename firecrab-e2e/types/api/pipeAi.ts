export interface AiConnection {
  id: string;
  name: string;
  provider: "openai" | "anthropic";
  authentication: "api_key" | "chatgpt";
  endpoint: string;
  models: string[];
  streaming: boolean;
  toolCalls: boolean;
}
export interface AiConnectionInput {
  name: string;
  provider: AiConnection["provider"];
  apiKey: string;
}

export interface AiLogin {
  loginId: string;
  authorizationUrl: string;
  expiresIn: number;
}
