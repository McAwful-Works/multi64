/**
 * EverDrive-64 X7 USB driver for the game-resident agent.
 *
 * EXPERIMENTAL: NO GAME HAS BEEN PLAYED THROUGH IT. On one X7 it has worked in n64/bringup's ROM,
 * outside any game: every request answered with the PI otherwise idle, and under PI load a reply
 * cut off part-way. ed64_send now retries a busy PI to prevent that, and with the retry the same
 * X7, in the same ROM, answered every request but one across three load levels
 * (l3-over-everdrive-x7.md 4.5 item 7). Inside a game it has answered on that X7 on 2026-10-12:
 * in an Ocarina of Time seed AP64 patched and left at its title screen, every HELLO and every
 * read, and later that day, by a tester's screenshot with no log, with an Archipelago client
 * connected. An earlier AP64 build, on 2026-10-07, opened the serial port and was never answered;
 * why is not known.
 *
 * All of that was before the receive path kept its place in the stream across calls and before any
 * wait was timed (below). Those two changes have run only against tests/ed64_test.c's fake cart:
 * no cart has run this driver as it now is.
 *
 * The console half of the DMA@ framing in docs/spec/l3-over-everdrive-x7.md section 4, written against libdragon's src/usb.c
 * (trunk c4a7e119), not copied from it. libdragon moves data by DMA and stages received packets
 * at the top of the ROM window; this driver does neither. Bytes go through the cart's 512-byte
 * USB window by CPU load and store (pi_io.c), so it owns no PI state and cannot land inside a
 * running game's ROM. A diagnostic build with PI_IO_DMA (make PI_IO=dma) moves the window by PI
 * DMA instead; see pi_io.c.
 *
 * Registers, PI bus:
 *   0x1F800004 USBCFG   USB mode in, status out
 *   0x1F800014 VERSION  0xED640013 (X7, X5) or 0xED640008 (3.0)
 *   0x1F800400 USBDAT   512-byte data window; a transfer of n bytes uses its last n bytes
 *   0x1F808000 SYSCFG
 *   0x1F808004 KEY      0xAA55 unlocks the registers
 *
 *
 * How long a call can take. Every wait ends by the CPU's Count register, read with mfc0 and never
 * written, as well as by a count of register reads; ed64.c has the limits. A call runs past its
 * limit by the pi_io call under way when the limit passes, and by the write that abandons a
 * transfer that call found overdue. pi_io.c bounds both itself: together, two waits for an idle PI
 * at most, about 53 ms at the 263 ns one PI status read was measured to take.
 *
 *   ed64_receive   100 ms and that overrun: about 0.15 s.
 *   ed64_send      250 ms and that overrun: about 0.3 s.
 *
 * agent.c calls ed64_receive until it holds a whole frame or gets 0 or a loss, up to 32 times a
 * tick, and then ed64_send at most once. The calls of one such run share 30 ms: once they have used
 * it the next returns 0, having begun nothing. So one agent_tick spends at most 30 ms and one
 * receive more in receiving, about 0.18 s, and about 0.49 s in the driver in all. Those are limits,
 * reached only when the PI stays busy or a transfer never finishes. On the one X7 measured, before
 * these limits existed, the longest agent_tick was 13.7 ms with the PI idle, and under the heaviest
 * PI load tried 61 to 65 ms in three runs and 305.8 ms in a fourth (n64/bringup/README.md, hardware
 * record).
 *
 * A run ends when a call returns 0 or a loss, and when ed64_send is called. A caller that stops on
 * a whole frame and sends no reply to it has not ended its run, so the time it used counts toward
 * the next one, which can then be told 0 once with a message waiting: a tick's delay, and no loss.
 */
#ifndef MULTI64_AGENT_ED64_H
#define MULTI64_AGENT_ED64_H

#include "m64p_types.h"

/**
 * Unlock the registers and confirm an X7 or 3.0 with its USB unit powered, forgetting whatever an
 * earlier use of the driver had read or was owed.
 * Returns 1 on success, 0 if this is not such a cart (an X5 has no USB).
 */
int ed64_init(void);

/** ed64_receive's result when bytes of the L3 stream were consumed and not delivered. */
#define ED64_RECEIVE_LOST 0xFFFFFFFFu

/**
 * Hand over L3 bytes the driver already holds, if any, before anything still on USB: as many as
 * `cap` allows, and a message ed64_send read ahead that was lost is reported as
 * ED64_RECEIVE_LOST where it fell.
 *
 * Otherwise carry on receiving one USB message, and hand over as much of it as `cap` allows once
 * it is whole. The driver puts a message together in its own 2 KiB first: the payload of a
 * well-formed DMA@ message carrying the L3 datatype is delivered only after its CMPH trailer has
 * been checked, and what `cap` leaves behind comes out of later calls before USB is read again.
 *
 * Returns 0 when nothing was delivered and nothing lost:
 * - nothing is waiting;
 * - the PI was busy for an access, or the call ran out of time. Wherever in a message that was,
 *   the driver keeps its place: bytes it has read stay in its buffer, bytes it has asked the cart
 *   for stay in the cart's USB window, and a later call carries on from there;
 * - the message is of another datatype. It is taken off USB unread, a call at a time, and its
 *   CMPH trailer goes unchecked with the rest: a bad one is found, if at all, as the next message's
 *   bad header;
 * - the calls of this run have used their 30 ms (above). Nothing was begun.
 *
 * Returns ED64_RECEIVE_LOST when a message was consumed but its L3 bytes were not delivered:
 * - its DMA@ header or CMPH trailer was bad;
 * - it carries more than 2 KiB of L3. The rest of it is drained unread, like a message of another
 *   datatype, so the messages behind it are not lost with it. crates/ed64-l2 sends 512 bytes a
 *   message;
 * - the cart never finished one of its reads, because the host sent less than the header said.
 * A message is reported lost once, and none of its bytes is delivered. A busy PI and the clock do
 * not lose one. After a bad header or a read that never finished, the driver does not know where
 * the next header is, and looks 8 bytes at a call.
 */
uint32_t ed64_receive(uint8_t *dst, uint32_t cap);

/**
 * Send `len` bytes as one DMA@ message of the L3 datatype. Returns 1 on success, 0 on timeout.
 *
 * A message goes out in 512-byte blocks. When the PI is busy before the first block starts, it
 * returns 0 having sent nothing. Once that block has started, a PI access that fails is made
 * again for as long as the call has time, so that a busy PI does not leave the host a message cut
 * off part-way; when the time runs out, it returns 0 with the message cut off.
 *
 * Reads every message the host has already sent first, keeping it for ed64_receive: an X7 does not
 * finish a write while host bytes wait unread (l3-over-everdrive-x7.md 4.5 item 6). When more is
 * waiting than the driver can hold (2 KiB), it returns 0 having sent nothing.
 */
int ed64_send(const uint8_t *data, uint32_t len);

#endif
