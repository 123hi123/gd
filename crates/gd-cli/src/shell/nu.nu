def --env gd [...args] {
    let first = ($args | get -i 0)
    let info_cmds = [link unlink list clean export init doctor version help hook -h --help -V --version]
    if ($first | is-not-empty) and ($first in $info_cmds) {
        ^gd ...$args
        return
    }
    if ($first == "-") {
        cd -
        return
    }
    let result = (^gd ...$args | str trim)
    if ($result | is-not-empty) { cd $result }
}
