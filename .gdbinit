#symbol-file target/riscv64gc-unknown-none-elf/debug/river
add-symbol-file ~/Code/RISCV/river/target/riscv64gc-unknown-none-elf/debug/userspace_testing
# disable this if you'd like
source ~/.gdbinit-gef.py
pi reset_architecture("RISCV")
# use this if not using gef
#target remote :1234
gef-remote --qemu-user localhost 1234

define hook-stop
# enable these if you'd like, I prefer gef
#p/a ($pc + 0xFFFFFFD000000000 - 0x80200000)
#list *($pc + 0xFFFFFFD000000000 - 0x80200000)
#x/5i $pc
end

define addrof
p/x &$arg0 - 0xFFFFFFD000000000 + 0x80200000
end

define set-break
break *($arg0 - 0xFFFFFFD000000000 + 0x80200000)
end

define symof
p/a ($arg0 + 0xFFFFFFD000000000 - 0x80200000)
end

define lst
list *($pc + 0xFFFFFFD000000000 - 0x80200000)
end

define p2v
print/x $arg0 + 0xFFFFFFD000000000 - 0x80200000
end

define v2p
print/x $arg0 - 0xFFFFFFD000000000 + 0x80200000
end

define until_ret
while(*(u16*)$pc != 0x8082)
ni
end
end

define until_ret_ni
while(*(u16*)$pc != 0x8082)
ni
end
ni
end

define sio
python
import gdb

class RunUntilCommand(gdb.Command):
    """Run until breakpoint and temporary disable other ones"""

    def __init__ (self):
        super(RunUntilCommand, self).__init__ ("run-until",
                                               gdb.COMMAND_BREAKPOINTS)

    def invoke(self, bp_num, from_tty):
        try:
            bp_num = int(bp_num)
        except (TypeError, ValueError):
            print("Enter breakpoint number as argument.")
            return

        all_breakpoints = gdb.breakpoints() or []
        breakpoints = [b for b in all_breakpoints
                       if b.is_valid() and b.enabled and b.number != bp_num and
                       b.visible == gdb.BP_BREAKPOINT]

        for b in breakpoints:
            b.enabled = False

        gdb.execute("c")

        for b in breakpoints:
            b.enabled = True

RunUntilCommand()

s = gdb.execute('x/3i $pc', to_string=True)
addr = s.split('\n')[2].strip().split(':')[0]
s = gdb.execute(f"b *{addr}", to_string=True)
bnum = s.split(' ')[1]
gdb.execute(f"run-until {bnum}")
gdb.execute(f"delete breakpoints {bnum}")
end
end
