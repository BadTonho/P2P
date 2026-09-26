# Roadmap do aplicativo P2P

Este roteiro divide o projeto em etapas para serem feitas uma de cada vez. Ao concluir uma etapa, você pode pedir: **“faça a etapa 1 do roadmap”**. Eu explico o que será feito e avanço para a próxima apenas quando você pedir.

## Escopo atual

- Aplicativo instalado primeiro no Windows.
- Rust como linguagem principal.
- egui/eframe para a interface gráfica.
- Chamada de voz e compartilhamento de tela.
- Salas limitadas a duas pessoas na primeira versão: você e um amigo.
- Sem câmera e sem chat de texto.
- Conexão P2P: áudio e tela devem ir diretamente entre os computadores quando a rede permitir.
- Servidor de sinalização rodando no notebook; ele ajuda os participantes a iniciar a conexão, sem repassar a mídia no modo P2P direto.
- TURN continua opcional. Se for usado como alternativa, retransmite a mídia e deixa de ser uma conexão direta.

## Etapas

### Etapa 0 — Fechar o escopo da primeira versão ✅ Concluída

Limite definido: duas pessoas por sala — você e um amigo. A primeira versão terá chamada de voz e compartilhamento de tela, sem câmera ou chat de texto, priorizando conexão P2P direta.

TURN permanece como decisão futura. Se for habilitado como alternativa, retransmitirá áudio e tela quando a conexão P2P direta falhar.

**Concluída quando:** limite e recursos da primeira versão estiverem definidos. ✅

### Etapa 1 — Preparar o projeto Rust

Verificar as ferramentas do notebook, organizar os projetos do aplicativo e do servidor e abrir uma janela mínima com egui/eframe.

**Concluída quando:** o aplicativo compilar e abrir no Windows.

### Etapa 2 — Montar a interface

Criar a tela da sala com nome ou código, estado da conexão e controles para iniciar/encerrar a chamada e compartilhar/parar a tela. Não haverá controles de câmera ou chat.

**Concluída quando:** os controles e estados visuais funcionarem, ainda sem transmissão pela rede.

### Etapa 3 — Testar microfone e captura de tela no próprio computador

Capturar o microfone e a tela localmente, pedir as permissões necessárias e permitir iniciar e parar cada captura.

**Concluída quando:** o aplicativo confirmar que consegue captar áudio e imagem e encerrar a captura corretamente.

### Etapa 4 — Criar o servidor de sinalização no notebook

Criar salas e permitir que os participantes troquem as informações usadas para negociar uma conexão WebRTC. O servidor não deve receber nem encaminhar áudio ou tela na conexão direta.

**Concluída quando:** dois aplicativos na mesma rede conseguirem entrar na sala e trocar as informações iniciais de conexão.

### Etapa 5 — Fazer a chamada P2P de voz na rede local

Conectar dois computadores na mesma rede Wi-Fi e transmitir o áudio diretamente entre eles usando WebRTC.

**Concluída quando:** ambos conseguirem falar e ouvir, e o servidor de sinalização não estiver encaminhando o áudio.

### Etapa 6 — Compartilhar a tela por P2P na rede local

Enviar a captura da tela diretamente ao outro participante e permitir parar o compartilhamento.

**Concluída quando:** o outro computador receber a tela e ela parar quando o usuário encerrar o compartilhamento ou a chamada.

### Etapa 7 — Conectar participantes em casas diferentes

Deixar o servidor de sinalização do notebook acessível pela internet e testar o estabelecimento de conexões diretas com ICE/STUN. Verificar as configurações do roteador e se o provedor permite conexões de entrada.

**Concluída quando:** dois participantes em redes diferentes conseguirem estabelecer voz e tela diretamente, sem o servidor de sinalização retransmitir mídia.

### Etapa 8 — Decidir o tratamento de redes que bloqueiam P2P

Se a conexão direta falhar em algumas redes, decidir entre manter o requisito estrito de P2P — e informar que a chamada não pode ser estabelecida — ou adicionar TURN como alternativa. TURN retransmite voz e tela e exige mais banda no servidor.

**Concluída quando:** a decisão sobre TURN estiver tomada e o aplicativo informar claramente quando a conexão é direta ou retransmitida.

### Etapa 9 — Preparar o uso e o instalador

Tratar encerramento de chamadas, desconexões, acesso às salas e estados de captura. Gerar o instalador do aplicativo cliente e preparar a execução do servidor no notebook.

**Concluída quando:** for possível instalar o cliente em outro computador e seguir os passos para conectar-se ao servidor do notebook.

## Como pedir a próxima etapa

Use o número e o nome da etapa, por exemplo: **“Vamos fazer a etapa 1 — Preparar o projeto Rust.”**
