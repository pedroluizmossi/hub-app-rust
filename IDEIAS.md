# Ideias

Ideias ainda não decididas nem começadas. O que já está planejado fica em
[ARCHITECTURE.md](ARCHITECTURE.md); aqui é rascunho. Anotadas em 2026-11-11.

## 1. Mascotes com memória (a principal)

Hoje um fato do `chatter` (`Trouble`, `Strain`, `Recovered`, `Poked`) vira uma fala e
some. A ideia é o hub guardar esses fatos num diário (arquivo de eventos com data) e o
diretor (`src/hub/chatter.rs`) consultar o diário antes de escolher a fala.

- **Memória e rancor.** "O postgres caiu de novo. Terceira vez essa semana." A mesma
  falha repetida muda o tom da fala.
- **Causa e efeito entre apps.** Os fatos já chegam todos ao diretor. Se a CPU esquenta
  segundos depois de um container subir, a mascote da CPU acusa a do Docker pelo nome. Se
  a porta 5432 some junto com o postgres, a mascote do Ports comenta.
- **Resumo do dia.** Ao abrir o popup de manhã (ou no fim do expediente), uma mascote
  narra: "Ontem: 2 quedas, pico de 94 °C às 15h, você fechou 5 tarefas."
- **Humor de longo prazo.** Uma semana sem alarme e a mascote fica convencida; muita
  cutucada e ela fica ranzinza.

Por que vale: usa o `chatter` que já existe, não pede provedor novo e não precisa de LLM
(um diário e algumas regras no diretor). As ideias 2 e o resumo do dia saem quase de
graça depois do diário.

Em aberto: formato e local do diário, por quanto tempo guardar, e como respeitar o
princípio "mini-app desligado não custa nada" (app desligado não registra fato).

## 2. Linha do tempo "o que aconteceu?"

Um app que desenha o diário da ideia 1 como faixa horizontal: CPU, containers, portas e
música alinhados no tempo. Serve para responder "por que travou às 15h?".

Depende da ideia 1.

## 3. Modo foco puxado pelo Todo

Marcar uma tarefa como "fazendo" inicia um pomodoro. Enquanto ele roda, as mascotes
ficam quietas; se a CPU ociosa ou a música parada indicarem que você saiu, a mascote do
Todo cobra.

Em aberto: o Todo passaria a assinar `sys` e `media` só para isso, ou o diretor é quem
cruza os sinais.

## 4. Mascote solta na tela

Usa o widget flutuante previsto na fase 2: em vez de um painel, a mascote do app em
alarme anda até a borda da tela e fica lá até o problema ser resolvido.

É a mais vistosa e a mais cara: janela transparente sem moldura se comporta diferente em
cada desktop, principalmente no Wayland.

## Ordem sugerida

1 → 2 → 3 → 4. O diário primeiro, porque destrava a linha do tempo e o resumo do dia.
