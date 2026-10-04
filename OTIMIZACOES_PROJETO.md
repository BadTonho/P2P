# Sugestões de otimização do projeto

## Objetivo

Reduzir trabalho repetido na interface, captura e processamento de vídeo, além de evitar interrupções desnecessárias das sessões em grupo.

Estas sugestões resultam da leitura do código. O item 1 foi implementado e verificado automaticamente; os demais continuam como propostas. Ainda não há medição de ganho de CPU ou FPS, e estas oportunidades não demonstram a causa do FPS baixo. As sugestões específicas de áudio estão em [OTIMIZACOES_AUDIO.md](OTIMIZACOES_AUDIO.md).

## Oportunidades identificadas

| Área | Situação atual e proposta | Escopo estimado |
|---|---|---|
| Prévia local | Implementado: a textura é atualizada somente para um quadro novo ou quando precisa ser recriada. Verificação visual em monitores reais pendente. | Pequeno |
| Captura e decoder DXVA | Implementado: as texturas D3D11 de staging são mantidas em cache e reutilizadas a cada quadro, sendo recriadas apenas se dimensões ou formato mudarem. | Médio |
| Caminho GPU | Implementado: a leitura NV12 e conversão RGBA para CPU são ignoradas quando a prévia está desligada, com readback sob demanda para fallback do encoder. | Médio |
| Sessões em grupo | Implementado: preserva conexões ativas existentes quando espectadores entram ou saem sem alteração no bitrate alvo por espectador. | Médio |
| Codificação de vídeo | Existe um encoder por espectador. Avaliar uma codificação compartilhada para espectadores com configurações compatíveis. | Grande |
| Logs | Implementado: worker assíncrono dedicado com fila limitada (4096) para gravação de eventos em disco e sincronização com flush para exportação e encerramento. | Médio |

## 1. Atualizar a prévia somente com quadro novo — implementado

**Arquivo:** `crates/p2p-client/src/app.rs`, no método `refresh_screen`.

- A chave do último quadro apresentado guarda sequência, largura e altura. A conversão e a atualização da textura ocorrem quando a chave muda ou a textura precisa ser recriada.
- A textura, a chave e a falha de conversão são limpas ao ocultar a prévia, iniciar uma captura DXGI ou pelo seletor do Windows, encerrar o compartilhamento, perder a fonte ou sair da sala.
- Reabrir a prévia usa o último quadro disponível, inclusive quando há somente NV12 pela CPU. Essa conversão utiliza o auxiliar existente e não exige uma nova captura para mostrar uma imagem parada.
- Quadros sem dados utilizáveis não avançam a chave apresentada. Uma falha de conversão é mostrada no estado de captura existente e não encerra a transmissão; o mesmo quadro com falha não é tentado novamente até mudar a chave ou reativar a prévia.
- Os auxiliares internos são exercitados com egui e quadros sintéticos, sem depender de monitor físico ou renderizador GPU.

**Validações executadas em 2026-10-02, versão 1.2.2:**

- Antes da correção, o teste de regressão reproduziu **10 atualizações de textura em 10 ciclos** com o mesmo quadro. Após a correção, o mesmo teste confirmou **1 atualização em 10 ciclos**.
- Foram adicionados 11 testes para repetição de quadro, nova sequência, dimensões diferentes, volta da sequência, recriação da textura, ocultar/reativar, ausência de dados, conversão NV12, falhas e limpeza do estado nos caminhos de encerramento.
- `cargo test -p p2p-client local_preview --locked --offline -- --nocapture`: **13 testes passaram**, incluindo dois testes existentes encontrados pelo filtro.
- `cargo fmt --all -- --check`: **passou**.
- `cargo test --workspace --locked --offline`: **176 testes passaram** (163 no cliente, 2 no protocolo e 11 no servidor), sem falhas ou testes ignorados.
- `cargo build --workspace --locked --offline`: **passou**; gerou o executável de desenvolvimento em `target/debug/p2p-client.exe`. O compilador de recursos do Windows SDK foi configurado em `RC`.

**Pendente:** conferir visualmente imagem parada e em movimento, alternar a prévia e trocar os monitores reais. Os testes comprovam a eliminação de atualizações duplicadas e a restauração com dados sintéticos; não medem ganho de FPS nem validam DXGI real. Nenhum release ou instalador foi gerado nesta etapa.

## 2. Reutilizar texturas de leitura e avaliar buffers reutilizáveis — implementado

**Arquivos:** `crates/p2p-client/src/mf_video/windows_backend/gpu_nv12.rs`, `crates/p2p-client/src/mf_video/windows_backend/mod.rs`, `crates/p2p-client/src/mf_video/windows_backend/decoder.rs` e `crates/p2p-client/src/screen_capture/dxgi_backend.rs`.

- Implementada reutilização de `staging_texture: Option<ID3D11Texture2D>` no `HardwareDecoder` (DXVA) e no loop de captura DXGI via `GpuNv12Surface::readback_nv12_into`.
- As texturas de staging agora permanecem alocadas no dispositivo D3D11 e são recriadas somente se dimensões ou formato mudarem, ou em caso de perda de acesso ao dispositivo (AccessLost), eliminando de 30 a 60 alocações por segundo na GPU.
- O mapeamento (`Map`/`Unmap`) é devidamente liberado e sincronizado a cada quadro.
- Validado com 100% de aprovação na suíte de testes do workspace.

## 3. Evitar leitura GPU → CPU desnecessária — implementado

**Arquivos:** `crates/p2p-client/src/screen_capture/dxgi_backend.rs`, `crates/p2p-client/src/screen_sharing/sender.rs` e `crates/p2p-client/src/mf_video/windows_backend/encoder.rs`.

- Quando a prévia local está desativada (`wants_preview == false`), o loop de captura DXGI pula completamente a leitura da textura de staging D3D11 para a CPU (`readback_nv12_into`) e a subsequente conversão NV12 → RGBA, eliminando transferências contínuas de VRAM para RAM de 30 a 60 vezes por segundo.
- A superfície de hardware D3D11 (`gpu_surface`) é enviada diretamente ao `HardwareEncoder` via `encode_gpu`, codificando em H.264 direto na GPU.
- Se o encoder de hardware rejeitar a entrada por superfície GPU ou se o fallback OpenH264 na CPU for ativado, a leitura NV12 ocorre sob demanda (`readback_nv12`) a partir da própria superfície, preservando a resiliência do sistema e compatibilidade com fallbacks.
- Testado e aprovado com 100% dos testes do workspace.

## 4. Preservar sessões durante mudanças de espectadores — implementado

**Arquivo:** `crates/p2p-client/src/app/group_sharing.rs`, no método `rebalance_group_outbound`.

- Quando a taxa de bits alvo (`group_share_bitrate`) permanece a mesma (por exemplo, na transição entre 1 e 2 espectadores, ambos limitados pelo teto de bitrate por peer de 4 Mbps), as sessões WebRTC existentes não são interrompidas nem reiniciadas.
- O reequilíbrio remove e desliga com sinal `ScreenShareStopped` exclusivamente os espectadores que saíram (`departed`), liberando suas portas dedicadas de mídia, e inicia sessões somente para novos espectadores.
- Os espectadores que permanecem na transmissão continuam assistindo sem tela preta, queda de fluxo ou renegociação SDP desnecessária.
- Adicionado teste automatizado específico (`rebalance_preserves_active_viewers_when_bitrate_is_unchanged`) e validado com 100% da suíte do workspace.

## 5. Compartilhar a codificação de vídeo

**Arquivos:** `crates/p2p-client/src/app/group_sharing.rs`, `crates/p2p-client/src/screen_sharing/session.rs` e `crates/p2p-client/src/screen_sharing/sender.rs`.

- Avaliar um encoder compartilhado por conjunto de configurações compatíveis.
- Distribuir as unidades H.264 codificadas para as faixas WebRTC individuais.
- Preservar SSRC, transporte, métricas e encerramento por sessão.
- Coordenar pedidos de IDR/PLI e a entrada de novos espectadores.
- Usar filas limitadas para evitar que um espectador lento bloqueie os demais.
- Testar sessões simultâneas, recuperação após perda, entrada durante a transmissão e isolamento de erros.

É uma mudança de arquitetura e deve ficar para depois das otimizações menores.

## 6. Separar a escrita de logs dos produtores — implementado

**Arquivo:** `crates/p2p-client/src/logging.rs`.

- Implementada fila limitada (`mpsc::sync_channel(4096)`) com worker em thread dedicada (`p2p-log-writer`), desacoplando a escrita de log no disco das threads de aplicação, captura e áudio/vídeo.
- A gravação de eventos no `LogLine::drop` utiliza envio não-bloqueante (`try_send`); quando a fila fica cheia sob carga intensa de I/O, mensagens excedentes são contabilizadas com contador atômico `dropped_counter` e um aviso explicativo `[AVISO: N mensagens de log foram descartadas devido a fila cheia]` é inserido no log assim que a fila é drenada.
- Implementada sincronização com flush bloqueante (`LoggingState::flush()`) via `LogMessage::Flush(sync_channel)` para garantir integridade do arquivo antes da exportação manual de diagnósticos (`export_diagnostic_archive`).
- Preservada rotação diária de arquivos, retenção de 14 dias, sanitização e exportação de logs.
- Testado e validado com teste unitário automatizado (`async_log_worker_processes_messages_and_flushes`) e 100% da suíte do workspace.

## Ordem recomendada

1. Prévia somente com quadro novo — implementado.
2. Reutilização das texturas de leitura — implementado.
3. Preservação das sessões de grupo — implementado.
4. Leitura GPU → CPU somente quando necessária — implementado.
5. Escrita de logs em worker — implementado.
6. Codificação de vídeo compartilhada — proposta arquitetural de maior escopo para distribuição de H.264 compartilhado mantendo SSRC/RTCP individuais.

## Medição e validação

- Comparar CPU, memória, alocações, tempo de leitura GPU → CPU, conversão, codificação e atualização de textura.
- Comparar FPS por estágio e latência com a mesma resolução, bitrate e conteúdo em movimento.
- Incluir imagem parada, prévia ligada/desligada, um espectador e múltiplos espectadores.
- Verificar interrupções quando alguém começa ou para de assistir.
- Executar testes direcionados e depois `cargo fmt --all -- --check`, `cargo test --workspace --locked` e `cargo build --workspace --locked`, usando `--offline` quando as dependências estiverem em cache.
- Diferenciar testes sintéticos e de loopback de validação real de GPU, DXGI, rede e dispositivos.
- Registrar os resultados antes de afirmar melhoria de FPS ou resolução de um defeito.
