# P2P — Voz e Tela 🦀🖥️

[![GitHub Release](https://img.shields.io/github/v/release/BadTonho/P2P?include_prereleases&color=orange&label=Vers%C3%A3o)](https://github.com/BadTonho/P2P/releases/latest)
[![Rust](https://img.shields.io/badge/Rust-2024%20Edition-red.svg?logo=rust)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Plataforma-Windows%2010%20%7C%2011-blue.svg?logo=windows)](https://microsoft.com/windows)
[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![DirectX 11](https://img.shields.io/badge/DirectX-11%20DXGI-0078D7.svg)](https://learn.microsoft.com/windows/win32/direct3ddxgi/desktop-dup-api)
[![WebRTC](https://img.shields.io/badge/WebRTC-P2P%20Direto-green.svg)](https://webrtc.org/)

Compartilhamento de tela em tempo real e transmissão de áudio do computador **diretamente entre amigos (Peer-to-Peer)**, com máxima performance, aceleração por GPU e privacidade absoluta. 

Construído 100% em **Rust**, sem servidores de nuvem intermediando o seu vídeo, sem contas obrigatórias e sem telemetria.

---

## 💡 A Ideia do Projeto

Plataformas tradicionais de comunicação costumam retransmitir o vídeo por servidores remotos de terceiros, reduzindo a taxa de quadros, comprimindo o áudio excessivamente e coletando dados de uso.

O **P2P — Voz e Tela** foi criado para oferecer uma alternativa direta, transparente e de altíssimo desempenho:

1. **Conexão Direta (P2P real):** O vídeo da sua tela e o som do seu computador trafegam diretamente do seu PC para o dos seus amigos via WebRTC (DTLS-SRTP criptografado).
2. **Sem Servidores Centrais de Mídia:** O próprio anfitrião roda o servidor de sinalização temporário (porta 9000). Nenhum dado de vídeo ou áudio passa por servidores da internet.
3. **Resiliência e Tolerância a Falhas:** Conta com uma malha de controle distribuída com **eleição automática de novo anfitrião** caso o host original fique instável ou caia.
4. **Privacidade por Padrão:** Não há cadastro, login, rastreadores ou armazenamento em nuvem. Suas configurações ficam salvas apenas no seu próprio computador.

---

## ✨ Recursos Principais

### 🚀 Desempenho e Gráficos de Baixa Latência
- **Captura Ultrarrápida via DirectX 11 (DXGI Desktop Duplication):** Captura de tela com latência submilisegundo diretamente da VRAM.
- **Aceleração por Hardware (GPU NVENC / Intel QuickSync / AMD AMF):** Codificação H.264 acelerada por hardware via Media Foundation com pipeline *zero-copy*.
- **Decodificação por Hardware DXVA2:** Reprodução fluida de vídeo remoto sem sobrecarregar a CPU.
- **Fallback Automático para CPU (OpenH264):** Garante compatibilidade caso o computador não possua GPU dedicada.
- **Controle Adaptativo de Taxa (Frame Pacer):** Transmite em até 60 FPS quando há movimento na tela e reduz inteligentemente a taxa em telas estáticas para economizar banda e processamento.

### 🎧 Áudio do Sistema de Alta Fidelidade
- **WASAPI Loopback de Baixa Latência:** Captura o som exato do que está tocando no Windows acionada por eventos do sistema.
- **Codec Opus 48 kHz Estéreo:** Qualidade de áudio cristalina para jogos, vídeos e música.
- **Isolamento Seletivo de Aplicativos:** Permite excluir o áudio de um aplicativo específico (ex: Discord, navegador ou jogo) para evitar eco ou sons indesejados no stream.

### 🌐 Conectividade e Malha de Controle
- **Salas para até 8 Participantes (Rede Local / Radmin VPN / WireGuard):** Cada participante pode transmitir sua tela e assistir às telas dos colegas em grade simultânea.
- **Modo Internet Direto (Teste):** Conexão direta via IP público/DDNS com suporte opcional a TURN integrado.
- **Sucessão Automática de Host:** Se a conexão do anfitrião oscilar (perda de pacotes, jitter alto ou queda), os participantes elegem um novo host em segundos, sem derrubar a sala.
- **Perfis Locais:** Nome e avatar personalizáveis (comprimidos e trafegados apenas dentro da sala local).

### 🛡️ Praticidade e Atualizações
- **Interface Gráfica Leve (GUI):** Desenvolvida em Rust puro com `egui` e `eframe` (inicia instantaneamente e consome pouca memória RAM).
- **Atualizador Integrado com GitHub Releases:** Notifica sobre novas versões, valida integridade (SHA-256 e assinatura PE) e aplica atualizações com rollback seguro em 1 clique.
- **Instalador sem privilégios de Admin:** Instalador Inno Setup por usuário ou executável portátil.

---

## 🏗️ Como Funciona a Arquitetura

```text
               +-----------------------------------+
               |  PC Anfitrião (Host Temporário)  |
               |  - Servidor de Sinalização (9000) |
               +-----------------+-----------------+
                                 |
                 Troca de SDP / ICE Candidates
                 (apenas metadados da conexão)
                                 |
        +------------------------+------------------------+
        |                                                 |
        v                                                 v
+-------------------------------+               +-------------------------------+
|      PC Participante A        |  Mídia WebRTC |      PC Participante B        |
|  - DXGI / NVENC (Captura GPU) |<=============>|  - DXVA2 (Decodificador GPU)  |
|  - WASAPI / Opus (Áudio)      | (P2P Direto)  |  - WASAPI (Reprodução Áudio)  |
|  - Malha de Controle (TCP 9001) <-----------> |  - Malha de Controle (TCP 9001)|
+-------------------------------+   Pulsos /    +-------------------------------+
                                    Eleição
```

- **Porta TCP 9000:** Usada apenas para sinalização inicial (troca de ofertas, respostas e candidatos ICE).
- **Porta TCP 9001:** Malha de controle descentralizada entre todos os membros (monitoramento de saúde da rede e eleição de anfitrião).
- **Portas UDP 9002–9009:** Mídia WebRTC de áudio e vídeo direta entre os computadores.

---

## 🚀 Como Usar

### 1. Download do Aplicativo
Baixe a versão mais recente na página de [Releases do GitHub](https://github.com/BadTonho/P2P/releases/latest):
- **Instalador recomendado:** `P2P-Voz-e-tela-Setup.exe` (instalação no seu usuário, atalho no menu Iniciar).
- **Portátil:** `p2p-client.exe` (basta baixar e rodar).

### 2. Criando uma Sala
1. Abra o **P2P - Voz e tela**.
2. Na tela inicial, escolha o modo de conexão:
   - **Rede local / Radmin:** Para computadores na mesma rede Wi-Fi/cabo ou conectados em redes virtuais como Radmin VPN / ZeroTier / Tailscale.
   - **Internet (teste):** Para conexões ponto a ponto diretas via IP público ou DDNS.
3. Clique em **Criar sala**.
4. Envie o endereço exibido (`ws://SEU-IP:9000`) e o código da sala para seus amigos.

### 3. Entrando na Sala
1. Abra o aplicativo.
2. Em **Configurações > Conexão**, insira o endereço do anfitrião (`ws://IP-DO-ANFITRIAO:9000`).
3. Na tela inicial, digite o código da sala e clique em **Entrar**.

### 4. Compartilhando Tela e Áudio
- Selecione o monitor ou janela desejada.
- *(Opcional)* Marque **"Incluir som do computador"** se quiser transmitir o áudio do Windows.
- Clique em **"Compartilhar minha tela"**. Os participantes conectados verão sua tela e poderão clicar em **"Assistir"**.

Para instruções completas e dicas de portas de roteador e firewall, consulte o guia [COMO_USAR.md](COMO_USAR.md).

---

## 🛠️ Compilando a partir do Código-Fonte

### Pré-requisitos
- [Rust](https://rustup.rs/) (Edição 2024 / versão 1.85 ou superior)
- Windows 10 (1803+) ou Windows 11
- Visual Studio C++ Build Tools ou Windows SDK (com `rc.exe` disponível para compilar os recursos de ícone/manifesto)
- [Inno Setup 6](https://jrsoftware.org/isdl.php) (opcional, apenas para compilar o instalador)

### Passos para Compilação

1. **Clone o repositório:**
   ```bash
   git clone https://github.com/BadTonho/P2P.git
   cd P2P
   ```

2. **Execute o cliente em modo de desenvolvimento:**
   ```bash
   cargo run --bin p2p-client
   ```

3. **Compile a versão otimizada de produção:**
   ```bash
   cargo build --release --locked
   ```
   O binário final estará em `target/release/p2p-client.exe`.

4. **Gerar o Instalador Windows (opcional):**
   ```powershell
   powershell -ExecutionPolicy Bypass -File scripts/build-installer.ps1
   ```
   O instalador gerado ficará em `target/installer/P2P-Voz-e-tela-Setup.exe`.

### Rodando os Testes
Para garantir que todos os componentes estão funcionando:
```bash
cargo test --workspace --locked
```

---

## 📁 Estrutura do Workspace

O projeto é organizado como um workspace Cargo modular em Rust:

```text
P2P/
├── crates/
│   ├── p2p-client/            # Cliente desktop com GUI (egui), WebRTC, captura DXGI,
│   │                          # Media Foundation, áudio WASAPI, malha e atualizador
│   ├── signaling-server/      # Servidor de sinalização WebSocket leve em Tokio/Axum
│   └── signaling-protocol/    # Tipos e mensagens de protocolo compartilhados (Serde)
├── installer/                 # Scripts do Inno Setup para empacotamento Windows
├── scripts/                   # Scripts utilitários de build e automação PowerShell
├── COMO_USAR.md               # Manual detalhado de uso e configurações de rede
├── ATUALIZACOES.md            # Guia de ciclo de vida e publicação de releases
└── DOCUMENTO_DO_PROJETO.md    # Especificações de design e requisitos do projeto
```

---

## 🔒 Privacidade e Segurança

- **Zero Coleta de Dados:** O aplicativo não possui telemetria, não se conecta a servidores de terceiros para enviar dados analíticos e não grava sessões.
- **Mídia Criptografada:** Toda a transmissão de áudio e vídeo WebRTC utiliza criptografia padronizada de ponta a ponta (DTLS e SRTP).
- **Configurações Locais:** Suas preferências são salvas exclusivamente em `%LOCALAPPDATA%\P2P-Voz-e-tela\settings.json`. Códigos de sala e credenciais temporárias nunca são persistidos no disco.

---

## 📄 Licença

Este projeto é software livre e de código aberto, distribuído sob a licença [GPL-3.0-or-later](LICENSE).
