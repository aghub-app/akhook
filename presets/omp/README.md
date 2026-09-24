# omp preset source

The 27 Markdown rules in this directory are copied unchanged from
[`can1357/oh-my-pi` v18.2.11](https://github.com/can1357/oh-my-pi/tree/v18.2.11/packages/coding-agent/src/discovery/builtin-rules)
at commit `e415159`. Their license is in [LICENSE](LICENSE).

akhook reads each rule's condition, AST patterns, path scopes, and Markdown body.
It maps a match to a pre-tool denial; omp's original `interruptMode: never`
soft-reminder behavior is not preserved.
