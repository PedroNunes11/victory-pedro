# Porte seletivo do ProjectNeptune — 28/11/2026

Base analisada: `PossiblyPix/ProjectNeptune` em `3f5b4d5abf32`.

## O que já existia

`victory-gpu` já possui copy-back por intervalos modificados em
`shadow.rs` (`back_piece` e `write_mapped_runs`). Portanto, não foi criado
um segundo mecanismo de cópia de memória, que aumentaria retenção e risco.

## Porte nesta branch

Branch: `experiment/neptune-port-2026-11-28`, baseada em `7b05d270fac9`.

* `VICTORY_SMALL_HOST=1`: reduz a retenção ociosa de listas de comando de
  256 MiB para 64 MiB e o cache de scratch de 128 MiB para 64 MiB. Em
  máquinas com até 24 GiB isso é automático; na máquina de 32 GiB continua
  desligado por padrão e é uma medida de folga, não de desempenho.
* `VICTORY_DYNAMIC_WAIT_MS=N`: espera, de forma bloqueante e sem polling, por
  slots de descritores dinâmicos devolvidos por listas concluídas. `0`
  preserva o comportamento atual de recusar a tabela; o limite é 1000 ms.
  O primeiro teste recomendado é `30` ms, somente se houver draws ausentes.

## Deliberadamente não portado

As alterações de envio paralelo, rastreamento direto de descritores e
DLSS/Frame Generation do Neptune são acopladas ao seu scheduler e/ou elevam
o consumo de VRAM. Não são um merge seguro para a RTX 4070 Ti SUPER antes de
uma medição isolada.

## Validação atual

`cargo check -p victory-gpu --lib --locked` não alcança esta alteração porque
a base `7b05d270fac9` já falha em `victory-shader`: há símbolos e constantes
duplicados/ausentes em `emit/shader.rs` e `lower/mod.rs`. Também havia um
delimitador ausente no `create_view` de `descriptor/host.rs`; ele foi corrigido
nesta branch. A falha restante é anterior a este porte e precisa ser resolvida
na integração de shaders antes da validação do crate GPU ou de um teste do jogo.
