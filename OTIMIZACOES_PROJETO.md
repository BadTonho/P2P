# Sugestões de otimização do projeto

## Objetivo

Reduzir trabalho repetido na interface, captura e processamento de vídeo, além de evitar interrupções desnecessárias das sessões em grupo.

Estas sugestões resultam da leitura do código atual. Ainda não foram implementadas nem medidas e não demonstram a causa do FPS baixo. As sugestões específicas de áudio estão em [OTIMIZACOES_AUDIO.md](OTIMIZACOES_AUDIO.md).

## Oportunidades identificadas

| Área | Situação atual e proposta | Escopo estimado |
|---|---|---|
| Prévia local | O mesmo quadro pode ser convertido e reenviado à GPU em vários ciclos da interface. Atualizar a textura somente quando chegar um quadro novo. | Pequeno |
| Captura e decoder DXVA | As texturas usadas para leitura pela CPU são criadas novamente a cada quadro. Reutilizá-las enquanto dispositivo, dimensões e formato permanecerem compatíveis. | Médio |
| Caminho GPU | A captura lê NV12 de volta para a CPU mesmo com a prévia desligada. Avaliar uma leitura somente quando necessária para a prévia ou para o fallback do encoder. | Médio |
| Sessões em grupo | O reequilíbrio encerra e recria todas as sessões de envio. Preservar as conexões existentes quando entra ou sai um espectador, considerando mudanças de bitrate. | Médio |
| Codificação de vídeo | Existe um encoder por espectador. Avaliar uma codificação compartilhada para espectadores com configurações compatíveis. | Grande |
| Logs | A escrita ocorre na própria thread que registra o evento. Avaliar uma fila limitada e um worker para escrita. | Médio |

## 1. Atualizar a prévia somente com quadro novo

**Arquivo:** `crates/p2p-client/src/app.rs`, no método `refresh_screen`.

- Guardar a sequência do último quadro enviado à textura local.
- Converter e atualizar a textura somente quando a sequência mudar.
- Limpar essa referência ao trocar ou encerrar a captura.
- Garantir que reabrir a prévia mostre o quadro atual, inclusive quando a imagem estiver parada.
- Testar quadro repetido, quadro novo, troca de captura e reativação da prévia.

## 2. Reutilizar texturas de leitura e avaliar buffers reutilizáveis

**Arquivos:** `crates/p2p-client/src/mf_video/windows_backend/gpu_nv12.rs`, `crates/p2p-client/src/mf_video/windows_backend/mod.rs` e `crates/p2p-client/src/mf_video/windows_backend/decoder.rs`.

- Manter as texturas de staging associadas ao componente responsável pela leitura.
- Recriá-las quando dispositivo, dimensões ou formato mudarem.
- Garantir sincronização e liberação do mapeamento em todos os caminhos de erro.
- Avaliar reutilização dos buffers NV12/RGBA respeitando os quadros ainda usados por outras threads.
- Testar mudanças de resolução, encerramento e fallback; validar o caminho D3D11 em hardware real.

Essa proposta busca reduzir alocações. Ela não comprova nem garante a correção do erro DXVA `0x8007000E` observado anteriormente.

## 3. Evitar leitura GPU → CPU desnecessária

**Arquivos:** `crates/p2p-client/src/screen_capture/dxgi_backend.rs`, `crates/p2p-client/src/screen_sharing/sender.rs` e `crates/p2p-client/src/mf_video/windows_backend/gpu_nv12.rs`.

- Avaliar a necessidade de leitura conforme a prévia e o caminho efetivamente usado pelo encoder.
- Preservar a leitura quando um encoder precisar de entrada pela CPU.
- Manter a detecção de quadros novos e o fallback após rejeição de uma superfície GPU.
- Testar prévia ligada/desligada, encoder GPU, encoder CPU e transição de fallback.

O mapeamento pode precisar esperar a GPU terminar de usar o recurso. Referência: [ID3D11DeviceContext::Map — Microsoft](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-map).

## 4. Preservar sessões durante mudanças de espectadores

**Arquivo:** `crates/p2p-client/src/app/group_sharing.rs`, no método `rebalance_group_outbound`.

- Criar sessões apenas para novos espectadores e encerrar apenas as removidas quando o bitrate das demais não mudar.
- Avaliar ajuste de bitrate durante a sessão quando o encoder oferecer suporte; definir o tratamento para encoders sem esse suporte.
- Preservar os limites atuais de banda e a correlação por geração.
- Testar entrada e saída de espectadores, sinais atrasados e continuidade das sessões mantidas.

## 5. Compartilhar a codificação de vídeo

**Arquivos:** `crates/p2p-client/src/app/group_sharing.rs`, `crates/p2p-client/src/screen_sharing/session.rs` e `crates/p2p-client/src/screen_sharing/sender.rs`.

- Avaliar um encoder compartilhado por conjunto de configurações compatíveis.
- Distribuir as unidades H.264 codificadas para as faixas WebRTC individuais.
- Preservar SSRC, transporte, métricas e encerramento por sessão.
- Coordenar pedidos de IDR/PLI e a entrada de novos espectadores.
- Usar filas limitadas para evitar que um espectador lento bloqueie os demais.
- Testar sessões simultâneas, recuperação após perda, entrada durante a transmissão e isolamento de erros.

É uma mudança de arquitetura e deve ficar para depois das otimizações menores.

## 6. Separar a escrita de logs dos produtores

**Arquivo:** `crates/p2p-client/src/logging.rs`.

- Avaliar um worker com fila limitada para gravação dos eventos.
- Definir o comportamento quando a fila estiver cheia e como contabilizar eventos não gravados.
- Preservar mudanças de nível em execução, rotação, retenção e exportação manual.
- Garantir que os eventos pendentes sejam tratados antes da exportação e do encerramento normal.
- Testar fila cheia, falha de escrita, ativação/desativação e encerramento.

O ganho deve ser medido especialmente com logs detalhados ativados.

## Ordem recomendada

1. Prévia somente com quadro novo.
2. Reutilização das texturas de leitura.
3. Preservação das sessões de grupo.
4. Leitura GPU → CPU somente quando necessária.
5. Escrita de logs em worker, conforme o custo medido.
6. Codificação de vídeo compartilhada.

Implementar cada mudança separadamente, com testes e comparação antes/depois.

## Medição e validação

- Comparar CPU, memória, alocações, tempo de leitura GPU → CPU, conversão, codificação e atualização de textura.
- Comparar FPS por estágio e latência com a mesma resolução, bitrate e conteúdo em movimento.
- Incluir imagem parada, prévia ligada/desligada, um espectador e múltiplos espectadores.
- Verificar interrupções quando alguém começa ou para de assistir.
- Executar testes direcionados e depois `cargo fmt --all -- --check`, `cargo test --workspace --locked` e `cargo build --workspace --locked`, usando `--offline` quando as dependências estiverem em cache.
- Diferenciar testes sintéticos e de loopback de validação real de GPU, DXGI, rede e dispositivos.
- Registrar os resultados antes de afirmar melhoria de FPS ou resolução de um defeito.
