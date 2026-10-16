# P2P — Voz e Tela 🦀🖥️

[![GitHub Release](https://img.shields.io/github/v/release/BadTonho/P2P?include_prereleases&color=orange&label=Vers%C3%A3o)](https://github.com/BadTonho/P2P/releases/latest)
[![Rust](https://img.shields.io/badge/Rust-2024%20Edition-red.svg?logo=rust)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Plataforma-Windows%2010%20%7C%2011-blue.svg?logo=windows)](https://microsoft.com/windows)
[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![DirectX 11](https://img.shields.io/badge/DirectX-11%20DXGI-0078D7.svg)](https://learn.microsoft.com/windows/win32/direct3ddxgi/desktop-dup-api)
[![WebRTC](https://img.shields.io/badge/WebRTC-P2P%20Direto-green.svg)](https://webrtc.org/)

Compartilhamento de tela em tempo real e transmissão de áudio do computador **diretamente entre amigos (Peer-to-Peer)**, com aceleração por GPU e mídia criptografada de ponta a ponta.

Construído 100% em **Rust**, sem servidores de nuvem intermediando o seu vídeo, sem contas obrigatórias e sem telemetria.

---

## 💡 A Ideia do Projeto

Plataformas tradicionais de comunicação costumam retransmitir o vídeo por servidores remotos de terceiros, reduzindo a taxa de quadros, comprimindo o áudio excessivamente e coletando dados de uso.

O **P2P — Voz e Tela** foi criado para oferecer uma alternativa direta, transparente e de altíssimo desempenho:

1. **Conexão Direta (P2P real):** O vídeo da sua tela e o som do seu computador trafegam diretamente do seu PC para o dos seus amigos via WebRTC (DTLS-SRTP criptografado), quando a rede permite um caminho direto. Se TURN estiver ativo e necessário, o tráfego de mídia será retransmitido pelo servidor TURN.
2. **Sem Servidores Centrais de Mídia:** O servidor de sinalização do anfitrião troca metadados de conexão, não mídia. Se a conexão direta falhar e TURN estiver ativo, o servidor TURN no PC anfitrião pode retransmitir áudio e vídeo.
3. **Resiliência e Tolerância a Falhas:** Conta com uma malha de controle distribuída com **eleição automática de novo anfitrião** caso o host original fique instável ou caia.
4. **Privacidade por Padrão:** Não há cadastro, login, rastreadores ou armazenamento em nuvem. As configurações ficam salvas apenas no seu computador. A sinalização usa `ws://` sem TLS por padrão; consulte a seção de segurança antes de usar pela Internet.

---

## ✨ Recursos Principais

### 🚀 Desempenho e Gráficos de Baixa Latência
- **Captura Ultrarrápida via DirectX 11 (DXGI Desktop Duplication):** Captura de tela com latência submilisegundo diretamente da VRAM.
- **Aceleração por Hardware (GPU NVENC / Intel QuickSync / AMD AMF):** Codificação H.264 acelerada por hardware via Media Foundation com pipeline *zero-copy*.
- **Decodificação por Hardware DXVA/Direct3D 11:** O receptor normaliza o NV12 usando o tamanho real da superfície e apresenta as cores por shader no OpenGL da interface. A cópia da GPU do decoder para a CPU permanece; se o shader falhar, a sessão usa apresentação RGBA. O desempenho depende do computador e do perfil selecionado.
- **Fallback Automático para CPU (OpenH264):** Garante compatibilidade caso o computador não possua GPU dedicada.
- **Busca manual de salas:** o botão **Buscar novamente** envia uma consulta na rede selecionada, preserva anúncios válidos e permite tentar recuperar a descoberta após falha. A busca automática permanece ativa.
- **Brilho estável do vídeo:** captura e apresentação opacas, com o mesmo contrato BT.601 limitado nos caminhos de conversão CPU/GPU e testes de interação.
- **Qualidade de transmissão configurável:** Antes de iniciar, escolha 720p ou 1080p e 30 ou 60 FPS. A preferência fica salva neste computador. A transmissão pode reduzir temporariamente bitrate, resolução e FPS e recuperá-los em etapas quando a rede e o receptor estabilizam, respeitando o perfil escolhido. O computador pode entregar menos FPS se não conseguir acompanhar. A taxa configurada é um alvo do encoder, não uma garantia de tráfego ou qualidade. Em grupo, o orçamento total é dividido entre quem está assistindo; veja os valores e limitações em [COMO_USAR.md](COMO_USAR.md).

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
- **Atualizador Integrado com GitHub Releases:** Notifica sobre novas versões, confere SHA-256 e a estrutura do executável PE e aplica atualizações com rollback.
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
- **Porta UDP 8999:** Descoberta de salas por broadcast no adaptador selecionado.
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
4. Em rede local/Radmin, mantenha **Anunciar sala nesta rede** ativo para seus amigos encontrarem a sala. Quem estiver nessa rede poderá entrar pelo anúncio sem código. Para entrada manual ou Internet, envie o endereço exibido (`ws://SEU-IP:9000`) e o código; a sinalização integrada usa `ws://` sem TLS.

### 3. Entrando na Sala
1. Abra o aplicativo.
2. Em **Rede local / Radmin**, escolha a sala em **Salas nesta rede** e clique em **Entrar**, sem digitar IP nem código. O app prioriza o Radmin ativo; na ausência dele, usa Ethernet/Wi-Fi. A descoberta exige UDP 8999 liberado no firewall e uma rede que transporte broadcast.
3. Para salas sem anúncio ou clientes antigos, use **Entrada manual · endereço e código**. Configure o endereço do anfitrião e informe o código. Use `wss://` somente com um endpoint TLS externo; o servidor integrado não termina TLS por conta própria.

### 4. Compartilhando Tela e Áudio
- Selecione o monitor ou janela desejada.
- *(Opcional)* Marque **"Incluir som do computador"** se quiser transmitir o áudio do Windows.
- Clique em **"Compartilhar minha tela"**. Os participantes conectados verão sua tela e poderão clicar em **"Assistir"**.

Para instruções completas e dicas de portas de roteador e firewall, consulte o guia [COMO_USAR.md](COMO_USAR.md).

---

## 🛠️ Compilando a partir do Código-Fonte

### Pré-requisitos
- [Rust](https://rustup.rs/) (Edição 2024 / versão 1.95 ou superior)
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

### Validando a apresentação NV12 na GPU

Depois de compilar, execute `target/release/p2p-client.exe --validate-nv12-gpu` (ou o executável de debug). Essa opção abre somente um contexto gráfico oculto, compara pixels do shader com a conversão CPU e verifica orientação, bordas, troca de resolução, reutilização e liberação de texturas. Não carrega nem salva as preferências do usuário. O resultado é indicado pelo código de saída e pela mensagem `NV12_GPU_VALIDATION_OK` ou `NV12_GPU_VALIDATION_FAILED` quando executado com saída capturada. Esse teste requer um contexto OpenGL nativo; não substitui a validação entre computadores.

### Validando brilho e interação

A opção `--validate-video-brightness` compara a conversão D3D11 com o caminho CPU usando preto, branco, cinzas e RGB, e testa alpha de captura 0, 96 e 255. Depois executa o shader e a apresentação real do egui em framebuffer, comparando pixels antes, durante e depois de pressionar o mouse nas dimensões de grade, ampliação e tela cheia. `--validate-nv12-gpu` também inclui essa verificação de interação. As imagens são sintéticas e as preferências não são carregadas nem salvas.

No PowerShell, use `Start-Process` com `-WindowStyle Hidden -Wait -PassThru` para aguardar esse executável gráfico e conferir a propriedade `ExitCode`. Quando a saída for capturada, o sucesso inclui `VIDEO_COLOR_VALIDATION_OK`, `VIDEO_CLICK_VALIDATION_OK` e `NV12_GPU_VALIDATION_OK`. A comparação CPU/GPU admite até quatro níveis por canal nas regiões internas das faixas por diferenças de arredondamento; a comparação entre estados de clique exige pixels idênticos. O teste precisa de D3D11 e OpenGL nativos.

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
│   ├── signaling-server/      # Servidor de sinalização WebSocket em Tokio/Tungstenite
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
- **Sinalização:** O anfitrião integrado anuncia `ws://` e não fornece TLS. Nesse modo, códigos de sala, metadados de conexão e credenciais TURN temporárias podem ser observados na rede. O cliente aceita `wss://` para um endpoint com TLS configurado externamente; isso protege o transporte até esse endpoint, mas não altera a criptografia da mídia WebRTC.

---

## 📄 Licença

Este projeto é software livre e de código aberto, distribuído sob a licença [GPL-3.0-or-later](LICENSE).
