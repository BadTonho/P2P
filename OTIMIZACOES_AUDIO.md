# Sugestões de otimização do áudio

## Objetivo

Reduzir o trabalho de captura e processamento de áudio, preservando a exclusão de aplicativos, os diagnósticos e o isolamento das sessões.

Os itens 1, 2 e 3 foram implementados e validados automaticamente na suíte de testes do workspace. O item 4 permanece documentado como proposta arquitetural de maior porte para distribuição de áudio multi-peer.

## 1. Atualizar métricas por bloco — implementado

**Arquivos:** `crates/p2p-client/src/audio_capture/process_loopback.rs` e `crates/p2p-client/src/audio_capture/system_audio.rs`.

- Os contadores de quadros capturados (`captured_frames`), descartados (`dropped_frames`) e amostras não silenciosas (`non_silent_samples`) são acumulados em variáveis locais durante o processamento do bloco.
- Uma única operação atômica `fetch_add` por métrica é executada ao final do bloco (e também no caminho de erro/aborto), eliminando até 48.000 operações atômicas por segundo por captura.
- Testado e validado automaticamente com a suíte completa de testes do workspace.

## 2. Capturar por eventos do Windows — implementado

**Arquivo:** `crates/p2p-client/src/audio_capture/process_loopback.rs`.

- Implementada inicialização orientada a eventos usando `AUDCLNT_STREAMFLAGS_EVENTCALLBACK` e vinculação via `IAudioClient::SetEventHandle`.
- O loop de captura substituiu o polling de 5 ms com `WaitForSingleObject` associado ao evento de buffer do WASAPI, com timeout de segurança e encerramento rápido.
- Ao sinalizar o evento, todos os pacotes de áudio acumulados são drenados imediatamente em um loop interno, eliminando latência de entrega e consumo ocioso de CPU quando o processo está mudo.
- O handle de evento do Windows é encapsulado em RAII (`EventHandle`) garantindo fechamento determinístico via `CloseHandle`.
- Testado e validado automaticamente com a suíte de testes do workspace.

## 3. Separar a detecção do aplicativo do envio de áudio — implementado

**Arquivos:** `crates/p2p-client/src/audio_capture/system_audio.rs`.

- O escaneamento de processos de aplicativo (`find_process_id` via snapshot do Windows Toolhelp32) foi movido para uma thread dedicada em background (`p2p-process-scan`), comunicando o PID detectado via `AtomicU32`.
- O método `read_samples`, executado a cada 10-20 ms pelo encoder Opus, realiza apenas uma leitura atômica sem contenção (`Ordering::Relaxed`), eliminando pausas e jitter causados por chamadas do sistema operacional no caminho crítico de áudio.
- O ciclo de vida do worker de escaneamento é cancelado de forma determinística no `Drop` de `SystemAudioCapture`.
- Testado e validado com a suíte de testes unitários do workspace.

## 4. Compartilhar a captura entre espectadores

**Arquivos:** `crates/p2p-client/src/app/group_sharing.rs` e `crates/p2p-client/src/screen_sharing/session.rs`.

Cada conexão de envio em grupo prepara sua própria captura e seu próprio encoder Opus. Com vários espectadores, esse trabalho é repetido.

- Avaliar uma captura e uma codificação Opus compartilhadas para a transmissão local.
- Distribuir os blocos codificados para as faixas WebRTC de cada espectador.
- Preservar transporte, SSRC, métricas e encerramento independentes por sessão.
- Usar filas limitadas para que um espectador lento não bloqueie os demais.
- Testar entrada e saída de espectadores, sessões simultâneas e isolamento de erros.

É a mudança de maior escopo e deve ficar para depois das otimizações menores.

## Ordem recomendada e status

1. Métricas por bloco — implementado.
2. Detecção e preparação da exclusão em worker — implementado.
3. Captura por eventos — implementado.
4. Captura e codificação compartilhadas entre espectadores — proposta para arquitetura futura.

## Medição e validação

- Comparar CPU, despertares da captura, tempo de processamento e latência antes e depois.
- Comparar quadros capturados e descartados, Opus enviado, RTP recebido e reprodução.
- Incluir silêncio, áudio contínuo, exclusão de aplicativo e múltiplos espectadores.
- Manter testes sintéticos e de loopback; eles não substituem a validação de WASAPI e áudio audível em PCs reais.
- Executar testes direcionados e depois `cargo fmt --all -- --check`, `cargo test --workspace --locked` e `cargo build --workspace --locked`, usando `--offline` quando as dependências estiverem em cache.
- Tratar qualquer ganho de FPS do vídeo como resultado a medir, pois depende de onde está o gargalo.
