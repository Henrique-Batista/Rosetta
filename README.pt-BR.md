🇧🇷 Português | 🇺🇸 [English](README.md)

# Rosetta — Proxy OpenAI-to-ACP

![Rosetta](assets/Rosetta%201.jpg)
![Rosetta](assets/Rosetta%202.png)

Rosetta é um proxy HTTP escrito em Rust que traduz entre a **Responses API** / **Chat Completions API** da OpenAI e o **Agent Client Protocol (ACP)**. Ele inicia um agente compatível com ACP (ex.: `opencode acp`) via stdio JSON-RPC 2.0 e expõe endpoints HTTP compatíveis com a OpenAI.

## Rosetta como Ponte

Pense em um modelo de IA como uma cabeça sem olhos, sem mãos, sem olfato — só consegue pensar, escutar e falar. Sozinha, ela não toca o mundo real; depende inteiramente de outra coisa para ser seu corpo. Pense na cabeça do RoboCop: essencial, central para o todo, mas inútil fora da armadura.

Um **agente** é o oposto: o mais próximo que existe de um ser totalmente autônomo. Tem braços, pernas, tronco — consegue ver, ouvir, tocar, se locomover. A melhor comparação é um ciborgue avançado, ou simplesmente uma pessoa: uma cabeça (o modelo) conectada a um sistema nervoso que lhe dá um corpo.

Esse sistema nervoso — o que decide *como* e *com o quê* o modelo alcança o mundo — é justamente o que são as tool calls, os servidores MCP e as skills. A cabeça decide *quando* agir; o sistema nervoso decide *como* a ação de fato acontece.

### Cabeças não encaixam em qualquer armadura

Uma pessoa também tem uma cabeça e um sistema nervoso próprio — mas não dá para simplesmente encaixar essa cabeça na armadura do RoboCop e sair andando por aí. Não existe interface para isso. A pessoa até conseguiria *conversar* com o RoboCop, mas *fazer* o que o RoboCop faz — pilotar aquele corpo específico — só uma cabeça compatível conseguiria. A armadura foi soldada em torno de uma única cabeça.

É exatamente por isso que trocar o *modelo* por trás de um agente costuma ser tão difícil: o harness geralmente é construído assumindo uma cabeça específica.

A parte de falar e escutar — a única interface que todo mundo realmente concorda em usar — é justamente a **Responses API / Chat Completions**: um padrão de linguagem compartilhado para conversar com um modelo. Uma pessoa também consegue se comunicar assim. Corpo diferente, mãos diferentes, mas o mesmo tipo de boca.

### Um podcast que só convida ciborgues

Agora imagine um podcast onde a única coisa que importa é falar e escutar — só que apenas quem está vestindo a armadura ciborgue do RoboCop ganha uma cadeira à mesa. Uma pessoa comum não consegue simplesmente entrar; no máximo, alguém de dentro repassa uma mensagem a ela depois.

É exatamente assim que os clientes de IA atuais funcionam: eles expõem um endpoint **Responses API / Chat Completions** e presumem que quem responde do outro lado é "um modelo". Um agente conseguiria manter essa mesma conversa perfeitamente, em linguagem natural — ele só nunca é convidado a entrar, porque não fala o dialeto esperado na porta.

### Aí entra o Rosetta: a armadura do Homem de Ferro

Agora troque a armadura soldada do RoboCop pela armadura do **Homem de Ferro**. Qualquer um pode vesti-la — uma pessoa, ou até outro ciborgue — desde que conheça os controles. E o mais importante: quem está dentro não deixa de ser *quem é*. Seus próprios instintos, seu conhecimento tácito (suas "skills"), coisas que já sabia fazer — tudo isso continua valendo, inclusive coisas para as quais a armadura do RoboCop nunca teve um encaixe.

**O Rosetta é essa armadura** — com um tanto de **Jarvis** embutido. Ele fala um protocolo um pouco diferente do que o Homem de Ferro costuma falar por dentro: o **Agent Client Protocol (ACP)**. É isso que permite conectar, e estender, tudo o que a armadura é capaz de fazer.

Onde o Homem de Ferro "pesquisaria" algo na própria memória (uma tool que só existe do lado do *agente*), o Jarvis consegue acessar um banco de dados ao qual o Tony tem acesso vindo de *fora* da armadura (uma tool que só existe do lado do *cliente*). Duas fontes de contexto diferentes, uma única conversa contínua.

Esse é o truque todo:
- A **Responses API / Chat Completions** garante que quem está vestindo a armadura ainda consegue falar com todo mundo do lado de fora, na língua que já conhecem.
- O **ACP** garante que qualquer um — qualquer modelo, qualquer cliente — consegue vestir a armadura e saber exatamente como pilotá-la.

Junte as duas coisas, e vestir a armadura deixa de exigir uma cabeça específica. Qualquer cabeça serve. Isso é o Rosetta.

## Sumário

- [Rosetta como Ponte](#rosetta-como-ponte) — uma analogia lúdica para o que o Rosetta faz
- [Instalação](#instalação)
- [Configuração](#configuração) — CLI e variáveis de ambiente, com precedência
- [Seleção de modelo e agente](#seleção-de-modelo-e-agente)
- [Testando com curl](#testando-com-curl)
- [Executando com o agente mock](#executando-com-o-agente-mock-para-testes)
- [Arquitetura](#arquitetura)
- [Estrutura do projeto](#estrutura-do-projeto)
- [Formato de resposta](#formato-de-resposta)
- [Debugging](#debugging)
- [Compatibilidade ACP](#compatibilidade-acp)
- [Notas importantes](#notas-importantes)
- [Roadmap](#roadmap)
- [Desenvolvimento](#desenvolvimento)

## Instalação

### Build

```bash
cargo build --release
```

O binário do servidor é gerado em `target/release/rosetta`.

## Configuração

Rosetta pode ser configurado de **duas formas**, que podem ser combinadas livremente:

1. **Flags de linha de comando** (`--acp-command`, `--acp-arg`, `--cwd`, `--mcp-servers`, `--listen`)
2. **Variáveis de ambiente** (`ROSETTA_ACP_COMMAND`, `ROSETTA_ACP_ARGS`, `ROSETTA_CWD`, `ROSETTA_MCP_SERVERS`, `ROSETTA_LISTEN`)

### Precedência (do maior para o menor)

```
1º  Flag de CLI          (--acp-command, --acp-arg, --cwd, --mcp-servers, --listen)
2º  Variável de ambiente (ROSETTA_ACP_COMMAND, ROSETTA_ACP_ARGS, ROSETTA_CWD, ROSETTA_MCP_SERVERS, ROSETTA_LISTEN)
3º  Valor padrão embutido
```

**A CLI sempre vence.** Se uma flag for passada explicitamente na linha de comando, o valor da variável de ambiente correspondente é ignorado — mesmo que ambas estejam definidas ao mesmo tempo.

### Referência de flags

| Flag CLI | Variável de ambiente | Padrão | Descrição |
|----------|----------------------|--------|-----------|
| `-c, --acp-command <COMMAND>` | `ROSETTA_ACP_COMMAND` | `opencode` | Comando usado para iniciar o agente ACP |
| `-a, --acp-arg <ARG>` (repetível) | `ROSETTA_ACP_ARGS` | `acp` | Argumento passado ao agente ACP. Pode ser repetido (`--acp-arg foo --acp-arg bar`) ou vir como string separada por espaços |
| `-w, --cwd <PATH>` | `ROSETTA_CWD` | diretório de trabalho atual do processo | Diretório de trabalho enviado ao agente em `session/new` |
| `-m, --mcp-servers <JSON>` | `ROSETTA_MCP_SERVERS` | `[]` (nenhum) | Array JSON com configurações de servidores MCP, repassado via `session/new`. JSON inválido aborta o processo com erro claro |
| `-l, --listen <HOST:PORT>` | `ROSETTA_LISTEN` | `0.0.0.0:3000` | Endereço/porta em que o servidor HTTP escuta |

Ver todas as opções e a documentação embutida:

```bash
./target/release/rosetta --help
```

### Exemplo — apenas variáveis de ambiente (compatível com versões anteriores)

```bash
ROSETTA_ACP_COMMAND=opencode \
ROSETTA_ACP_ARGS="acp" \
./target/release/rosetta
```

### Exemplo — apenas flags de CLI

```bash
./target/release/rosetta \
  --acp-command opencode \
  --acp-arg acp \
  --listen 0.0.0.0:3000
```

### Exemplo — múltiplos argumentos ao agente via CLI

```bash
./target/release/rosetta \
  --acp-command opencode \
  --acp-arg acp \
  --acp-arg --verbose
```

### Exemplo — servidores MCP via CLI

```bash
./target/release/rosetta \
  --mcp-servers '[{"name":"fs","command":"mcp-fs"}]'
```

### Exemplo — CLI sobrepondo variáveis de ambiente

```bash
# ROSETTA_ACP_COMMAND=python3 está definido no ambiente,
# mas --acp-command opencode na CLI tem prioridade e vence.
ROSETTA_ACP_COMMAND=python3 \
./target/release/rosetta --acp-command opencode --acp-arg acp
# Resultado: o agente iniciado é "opencode acp", não "python3"
```

## Seleção de modelo e agente

Rosetta permite selecionar tanto o **modelo LLM** quanto o **modo do agente** usando o campo `model` da requisição HTTP.

**Sintaxe:** `model:agente` (ex.: `opencode/gpt-5:sisyphus`)
- A parte **antes** de `:` seleciona o modelo LLM
- A parte **depois** de `:` seleciona o agente/modo (opcional)

**Exemplo — usando um modelo específico:**

```bash
curl http://localhost:3000/v1/responses \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "model": "opencode/gpt-5",
    "input": [
      {"type": "message", "role": "user", "content": "Hello"}
    ]
  }'
```

**Exemplo — usando modelo + agente específicos:**

```bash
curl http://localhost:3000/v1/responses \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "model": "opencode/gpt-5:sisyphus",
    "input": [
      {"type": "message", "role": "user", "content": "Build a web server"}
    ]
  }'
```

Modelos e agentes disponíveis dependem da configuração do seu agente ACP. Prefixos comuns:
- `opencode/` — agentes do OpenCode Zen (ex.: `opencode/gpt-5`, `opencode/claude-sonnet-4-5`)
- `opencode-go/` — agentes do OpenCode Go (ex.: `opencode-go/kimi-k2.6`)
- `openrouter/` — modelos via OpenRouter (ex.: `openrouter/anthropic/claude-opus-4`)
- `google/` — modelos Google (ex.: `google/gemini-2.5-pro`)
- `groq/` — modelos Groq

**Como funciona:**
1. Rosetta inicia o agente ACP **sem injetar nenhuma configuração específica do agente** — o agente usa sua própria configuração (arquivos de config, variáveis de ambiente, etc.)
2. Rosetta interpreta o campo `model` para extrair modelo e agente (ex.: `opencode/gpt-5:sisyphus`)
3. Após `session/new`, Rosetta inspeciona `configOptions` na resposta ACP
4. Se uma opção `category: "mode"` corresponder ao agente solicitado, Rosetta chama `session/set_config_option`
5. Servidores MCP podem ser passados ao agente via a flag `--mcp-servers` / variável `ROSETTA_MCP_SERVERS`
6. Isso é **totalmente agnóstico a ACP** — qualquer agente ACP funciona sem que Rosetta assuma nada sobre sua configuração interna

## Testando com curl

**Responses API:**

```bash
curl http://localhost:3000/v1/responses \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "input": [
      {"type": "message", "role": "user", "content": "Hello"}
    ]
  }'
```

**Chat Completions API:**

```bash
curl http://localhost:3000/v1/chat/completions \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "messages": [
      {"role": "user", "content": "Hello"}
    ]
  }'
```

## Executando com o agente mock (para testes)

Um agente mock em Python está incluído para testes de integração:

```bash
./target/release/rosetta \
  --acp-command python3 \
  --acp-arg crates/rosetta-acp/tests/fixtures/mock_acp.py
```

**Nota:** ao usar o agente mock, o campo `model` é ignorado. O agente mock sempre retorna uma resposta fixa.

## Arquitetura

```
┌─────────────┐      HTTP/JSON       ┌──────────────┐      stdio/NDJSON      ┌─────────────┐
│   Cliente   │  ──────────────────> │   Rosetta    │  ───────────────────>  │  Agente ACP │
│  (OpenAI    │   /v1/responses      │   Servidor   │   JSON-RPC 2.0        │ (opencode   │
│    SDK)     │   /v1/chat/completions│  (Axum)     │   initialize          │   acp)      │
└─────────────┘                      └──────────────┘   session/new           └─────────────┘
                                                      session/prompt
                                                      session/update (streaming)
                                                      session/close
```

A configuração (CLI + env) é resolvida uma única vez em `main()` (`crates/rosetta-server/src/cli.rs`) antes do servidor HTTP subir, e o resultado (`ResolvedConfig`) alimenta o `AppState` compartilhado que cada requisição usa para iniciar um cliente ACP.

## Estrutura do projeto

| Crate | Responsabilidade |
|-------|--------------|
| `rosetta-types` | Tipos de requisição/resposta OpenAI e ACP |
| `rosetta-acp` | Cliente JSON-RPC 2.0 + transporte via stdio |
| `rosetta-core` | Camada de tradução entre OpenAI e ACP |
| `rosetta-server` | Servidor HTTP Axum + CLI (`clap`) + handlers de rota |

## Formato de resposta

Rosetta traduz as atualizações do agente ACP em estruturas de resposta compatíveis com a OpenAI:

| Tipo de atualização ACP | Saída OpenAI | Descrição |
|----------------|---------------|-------------|
| `agent_thought_chunk` | `OutputItem::Reasoning` (type: `reasoning`, summary_type: `thinking`) | Raciocínio interno do modelo |
| `agent_message_chunk` | `OutputItem::Message` (type: `message`) | Texto final voltado ao usuário |
| `tool_call` | `OutputItem::Reasoning` (type: `reasoning`, summary_type: `tool_call`) | Invocação de ferramenta pelo agente (exposta como reasoning, não como function call) |
| `available_commands_update` | *(descartado silenciosamente — log em nível debug)* | Agente anunciando comandos/skills disponíveis |
| outros tipos | *(descartado silenciosamente — log em nível debug)* | Tipos de atualização não tratados |

O campo `output_text` na resposta contém **apenas texto de mensagem** (sem texto de raciocínio/thinking).

## Debugging

Rosetta usa logging estruturado via o crate `tracing`. Defina `RUST_LOG` para controlar a verbosidade:

```bash
# Mostrar apenas invocações de tool/skill
RUST_LOG=rosetta_core=info ./target/release/rosetta

# Mostrar todos os tipos de atualização (incluindo os descartados)
RUST_LOG=rosetta_core=debug ./target/release/rosetta

# Mostrar o JSON completo de cada atualização de sessão ACP
RUST_LOG=rosetta_core=trace ./target/release/rosetta
```

### Níveis de log

| Nível | O que você vê | Caso de uso |
|-------|-------------|----------|
| `info` | `ACP tool_call received — agent invoked a tool/skill` | Confirmar que uma skill/tool foi chamada |
| `debug` | `agent_thought_chunk received`, `Unhandled ACP session update type` | Ver quais tipos de atualização o agente envia |
| `trace` | Corpo JSON completo de cada atualização ACP | Depurar comunicação bruta do protocolo ACP |

## Compatibilidade ACP

Rosetta é construído sobre o **Agent Client Protocol (ACP)**, definido de fato pela implementação ACP do opencode. Abaixo, uma avaliação de compatibilidade para outros agentes ACP além do opencode.

### Camada de protocolo

| Camada | Status | Detalhes |
|-------|--------|---------|
| **Transporte** | 🟢 Compatível com ACP | JSON delimitado por linhas sobre stdio. Padrão para ACP. |
| **Initialize** | 🟢 Compatível com ACP | `initialize` com `protocolVersion` — JSON-RPC 2.0 genérico. O campo `serverInfo` aceita o alias `agentInfo` para compatibilidade retroativa. |
| **Ciclo de vida da sessão** | 🟢 Compatível com ACP | `session/new` → `session/prompt` → `session/close`. Fluxo padrão. |
| **Servidores MCP** | 🟢 Compatível com ACP | Passados via campo padrão `mcpServers` em `session/new`. |
| `session/set_config_option` | 🟡 Alinhado ao opencode | Este método é definido na spec ACP mas implementado principalmente pelo opencode. Outros agentes podem não suportá-lo. Rosetta trata a ausência com elegância (log, sem crash). |

### Formato de atualização

| Aspecto | Status | Detalhes |
|--------|--------|---------|
| **Localização do tipo de atualização** | 🟡 Alinhado ao opencode | Rosetta verifica DUAS localizações: `body.updateType` (formato flat) e `body.update.sessionUpdate` (formato aninhado). Um agente usando um terceiro formato teria todas as atualizações descartadas silenciosamente. |
| **Localização do payload de dados** | 🟡 Alinhado ao opencode | Rosetta verifica `body.data` e `body.update`. Mesma abordagem dual acima. |
| **Nomes de tipos de atualização** | 🔴 Específico do opencode | Apenas `agent_thought_chunk`, `agent_message_chunk` e `tool_call` são reconhecidos. Outros tipos (ex.: `agent_message`, `tool_call_update`, `user_message_chunk`, `plan`, `current_mode_update`) são descartados silenciosamente — log em nível debug. |
| **Estrutura do campo content** | 🟡 Alinhado ao opencode | Extrai texto de `content.type=="text" && content.text` (aninhado) ou `content`/`text` como string plana (flat). |
| **Campos de tool call** | 🔴 Específico do opencode | Espera `toolCallId`, `title`, `name`, `arguments` (e fallback `params`). Outros agentes podem usar nomes de campo diferentes. |

### Conteúdo e prompt

| Aspecto | Status | Detalhes |
|--------|--------|---------|
| **Mensagem OpenAI → prompt ACP** | 🟡 Alinhado ao opencode | Prefixa mensagens com `[System]\n`, `[Assistant]\n`, `[Tool Result]\n` — convenções do opencode. Outros agentes ACP podem não entender esses marcadores. |
| **Tipos de conteúdo** | 🟡 Compatível com ACP | Apenas `ContentBlock::Text` é gerado. Partes de conteúdo `InputImage` e `InputFile` são descartadas silenciosamente. |
| **Ordem das mensagens de chat** | 🟡 Compatível com ACP | Mensagens são traduzidas em ordem com prefixos de role. Comportamento padrão. |

### Configuração

| Aspecto | Status | Detalhes |
|--------|--------|---------|
| **Injeção de configuração do agente** | 🟢 Compatível com ACP | Rosetta NÃO injeta nenhuma configuração específica do agente (ex.: `OPENCODE_CONFIG`). O agente usa sua própria configuração naturalmente. |
| **Seleção de modelo/agente** | 🟡 Alinhado ao opencode | A sintaxe `model:agente` (ex.: `opencode/gpt-5:sisyphus`) é extraída do campo `model` da OpenAI. Após `session/new`, Rosetta inspeciona `configOptions` e chama `session/set_config_option` se uma opção `mode` correspondente for encontrada. Agentes sem `configOptions` simplesmente usarão seu padrão. |
| **Variáveis de ambiente** | 🟢 Compatível com ACP | Usa variáveis com prefixo `ROSETTA_*`. Nenhuma variável específica de agente é injetada. |

### Funcionalidades ausentes

| Funcionalidade | Impacto | Detalhes |
|---------|--------|---------|
| **Loop de execução de ferramentas** | 🔴 Específico do opencode | Quando o agente faz um `tool_call`, Rosetta converte para um item de saída `Reasoning`. Não há loop para executar a ferramenta e enviar os resultados de volta ao agente. Isso significa que fluxos dependentes de ferramentas (ex.: busca web, operações de arquivo) não serão concluídos. |
| **Conteúdo multimodal** | 🟡 Compatível com ACP | `InputImage` e `InputFile` são descartados. Apenas `InputText` é repassado. Um agente esperando imagens ou arquivos não os receberá. |
| **Relatório de uso de tokens** | 🟡 Compatível com ACP | Atualmente fixo em zero. O campo `usage` de `PromptResponse` do agente ACP está disponível mas ainda não é interpretado. |

### Resumo

| Nível | Definição | Cobertura |
|-------|-----------|----------|
| 🟢 **Compatível com ACP** | Funciona com qualquer agente ACP que respeite o protocolo | Transporte, init, ciclo de vida da sessão, servidores MCP, variáveis de ambiente |
| 🟡 **Alinhado ao opencode** | Testado com opencode; provavelmente funciona com outros com pequenos ajustes | Formato de atualização, estrutura de conteúdo, opções de configuração |
| 🔴 **Específico do opencode** | Só funciona com opencode | Nomes de tipos de atualização, campos de tool call, loop de execução de ferramentas |

**Resumo final:** um agente ACP genérico que implemente o protocolo básico (initialize → session/new → session/prompt → session/update → session/close) funcionará para conversas de texto básicas. Funcionalidades como execução de ferramentas, entrada multimodal e tratamento de tipos de atualização específicos são específicas do opencode e exigiriam adaptação.

## Notas importantes

- **Parâmetros de runtime** (`temperature`, `top_p`, etc.) são ignorados conforme a spec ACP — não são repassados ao agente.
- **Streaming**: para requisições com `stream: true`, ambas as APIs agora entregam o conteúdo em tempo real, conforme o agente ACP o produz, em vez de coletar todo o turno primeiro:
  - Responses API e Chat Completions consomem `AcpClient::send_prompt_streaming()` através de um canal limitado (bounded) de propriedade da task (`rosetta-server/src/streaming_task.rs`), traduzindo cada `session/update` em um evento/chunk SSE assim que ele chega
  - Requisições não-streaming (`stream: false`) não são afetadas e continuam usando as versões em modo batch `response_to_streaming_events()`/`response_to_chat_chunks()`, `response_to_chat_completion()`
- **Servidores MCP** são passados através do campo padrão-ACP `mcpServers` em `session/new` — configure via flag `--mcp-servers` ou variável `ROSETTA_MCP_SERVERS`
- O enum `InputItem` exige `"type": "message"` no array de input.
- Nomes de campo ACP usam `camelCase` (ex.: `protocolVersion`, `sessionId`).
- Partes de input do `Client` que não sejam `input_text` (ex.: `input_file`, `input_image`) são descartadas silenciosamente durante a tradução do prompt.

## Roadmap

### Limitações conhecidas e trabalho futuro

| Item | Descrição | Status |
|------|-------------|--------|
| **Avaliação de gatilhos de skill em modo ACP** | Skills de `~/.opencode/skills/` são carregadas e anunciadas via `available_commands_update`, mas o agente ACP não avalia as condições de gatilho do SKILL.md automaticamente. Em modo CLI, o opencode verifica os gatilhos antes de montar o prompt do LLM. Em modo ACP, essa lógica não é executada. Precisa ser implementado no agente ACP (opencode), não no Rosetta. | 🔜 Futuro (lado opencode) |
| **Suporte a arquivo/imagem de entrada** | Partes de conteúdo `InputFile` e `InputImage` na requisição OpenAI são descartadas durante a tradução do prompt. Apenas partes `InputText` são repassadas ao agente ACP. | 📋 Planejado |
| **Streaming verdadeiro para a Responses API** | O caminho SSE atual coleta todas as atualizações primeiro, depois gera eventos a partir da resposta finalizada. Um caminho de streaming verdadeiro usando `send_prompt_streaming()` existe em `AcpClient` mas ainda não está conectado ao handler de rota HTTP (exige arquitetura baseada em canais). | 📋 Planejado |
| **Rastreamento de uso de tokens** | O uso atual é fixo em `{input_tokens: 0, output_tokens: 0, total_tokens: 0}`. O campo `usage` de `PromptResponse` do agente ACP está disponível mas ainda não é interpretado. | 📋 Planejado |
| **Loop de execução de tool call** | Quando o agente faz um `tool_call`, Rosetta o converte em um item de saída `Reasoning`. Não há loop para executar a ferramenta e enviar os resultados de volta ao agente. | 🔜 Futuro |

## Desenvolvimento

### Rodar todos os testes

```bash
cargo test --workspace
```

### Rodar apenas os testes unitários

```bash
cargo test -p rosetta-core
```

### Rodar os testes da CLI (`rosetta-server`)

```bash
cargo test -p rosetta-server
```

### Rodar teste de integração com o agente mock

```bash
cargo test -p rosetta-acp --test integration_test
```

### Rodar com logging de debug

```bash
RUST_LOG=rosetta_core=debug cargo run
```

## Licença

MIT
