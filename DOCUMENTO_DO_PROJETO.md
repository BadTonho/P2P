# Documento inicial do projeto

## Objetivo

Criar um aplicativo instalado no computador para conversar com amigos e compartilhar a tela.

O aplicativo será desenvolvido primeiro para Windows e distribuído como instalador. Cada participante usará o programa instalado no próprio computador. O servidor ficará rodando no notebook do dono do aplicativo.

## Primeira versão

O primeiro protótipo deve permitir:

- abrir o aplicativo e informar um nome de exibição;
- criar ou entrar em uma conversa privada com amigos;
- trocar mensagens de texto;
- iniciar e parar o compartilhamento da tela;
- encerrar a conversa.

O aplicativo deve pedir permissão antes de transmitir a tela e mostrar claramente quando o compartilhamento estiver ativo.

## Linguagem escolhida

- **Rust** será a linguagem principal do projeto, incluindo o aplicativo e o servidor hospedado no notebook.
- **egui e eframe** serão usados para montar a interface do aplicativo em Rust, sem precisar criar a interface em HTML, CSS ou JavaScript.
- **WebRTC** poderá ser usado para a comunicação em tempo real. WebRTC é uma tecnologia de comunicação, não uma linguagem.

## Etapas de desenvolvimento

1. Preparar o projeto no notebook com Windows.
2. Criar as telas básicas do aplicativo.
3. Implementar uma conversa simples e testar os recursos localmente.
4. Testar a conexão entre computadores na mesma rede Wi-Fi.
5. Gerar um instalador do aplicativo.
6. Estudar a conexão entre amigos em redes diferentes e escolher a solução adequada.

## Servidor e conexão entre os participantes

O notebook do dono hospedará e executará o servidor do aplicativo. Os computadores dos amigos se conectarão a esse servidor para conversar e compartilhar tela. O notebook precisará estar ligado e conectado à internet quando os amigos forem usar o aplicativo.

A primeira prova de funcionamento será feita entre computadores na mesma rede Wi-Fi. Depois, vamos configurar e testar conexões entre casas diferentes.

Ainda precisamos decidir se o servidor no notebook encaminhará todo o áudio e a imagem da tela ou se apenas organizará a conexão para que os computadores transmitam esses dados diretamente entre si. Encaminhar toda a transmissão pelo notebook exige mais velocidade de envio da internet dele.

## Fora do escopo inicial

- publicar um site;
- criar feed, grupos públicos ou outras funções de rede social;
- adicionar câmera ou chamadas de vídeo;
- adicionar contas, lista de amigos ou histórico permanente de mensagens.

Esses itens só serão considerados se forem necessários depois do primeiro protótipo.

## Pontos a decidir depois

- A conversa será só por texto ou também por voz?
- A sala será entre duas pessoas ou poderá ter vários amigos?
- O servidor no notebook encaminhará todo o áudio e a tela ou apenas ajudará os computadores a se conectarem diretamente?
- As mensagens precisam ficar salvas depois que a conversa terminar?
