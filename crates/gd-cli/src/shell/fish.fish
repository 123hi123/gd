function __gd_hook --on-variable PWD
    command gd hook "$PWD" &>/dev/null &
end

function gd
    switch $argv[1]
        case link unlink list clean export init doctor version help hook '-h' '--help' '-V' '--version'
            command gd $argv
            return $status
        case '-'
            cd -
            return $status
    end

    set -l result (command gd $argv)
    or return $status
    test -n "$result" && cd $result
end

complete -c gd -f -a "link unlink list clean export init doctor help"
