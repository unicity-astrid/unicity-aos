//! Deterministic RV64F/RV64D execution.
//!
//! Arithmetic is delegated to LLVM's pure-Rust APFloat port and the matching
//! correctly-rounded square-root implementation. Architectural NaN boxing,
//! canonical NaNs, exception flags, rounding-mode admission, and integer
//! saturation remain explicit machine policy here.

use super::*;
use std::cmp::Ordering;

impl Machine {
    pub(super) fn execute_float_load(
        &mut self,
        instruction: u32,
        rd: usize,
        rs1: usize,
        funct3: u32,
        pc: u64,
    ) -> Result<(), MachineTrap> {
        self.ensure_floating_enabled(pc, instruction)?;
        let bytes = match funct3 {
            0b010 => 4,
            0b011 => 8,
            _ => return Err(illegal(pc, instruction)),
        };
        let address = self.cpu.read(rs1).wrapping_add(immediate_i(instruction));
        ensure_aligned(address, bytes, false)?;
        let physical = self.translate(address, AccessType::Load)?;
        let value = self
            .devices
            .read(physical, bytes)
            .map_err(|_| MachineTrap::LoadAccessFault { address, bytes })?;
        if bytes == 4 {
            self.cpu.write_float32(rd, value as u32);
        } else {
            self.cpu.write_float64(rd, value);
        }
        self.csrs.mark_float_dirty();
        Ok(())
    }

    pub(super) fn execute_float_store(
        &mut self,
        instruction: u32,
        rs1: usize,
        rs2: usize,
        funct3: u32,
        pc: u64,
    ) -> Result<Option<HaltStatus>, MachineTrap> {
        self.ensure_floating_enabled(pc, instruction)?;
        let bytes = match funct3 {
            0b010 => 4,
            0b011 => 8,
            _ => return Err(illegal(pc, instruction)),
        };
        let address = self.cpu.read(rs1).wrapping_add(immediate_s(instruction));
        ensure_aligned(address, bytes, true)?;
        let physical = self.translate(address, AccessType::Store)?;
        self.invalidate_reservation(physical, bytes);
        let value = if bytes == 4 {
            self.cpu.floating_registers[rs2] & u64::from(u32::MAX)
        } else {
            self.cpu.read_float64(rs2)
        };
        let halt = self
            .devices
            .write(physical, value, bytes)
            .map_err(|trap| map_store_fault(trap, address, bytes))?;
        self.csrs.mark_float_dirty();
        Ok(halt)
    }

    pub(super) fn execute_float_fused(
        &mut self,
        instruction: u32,
        opcode: u32,
        pc: u64,
    ) -> Result<(), MachineTrap> {
        self.ensure_floating_enabled(pc, instruction)?;
        let format = (instruction >> 25) & 0b11;
        if format > 1 {
            return Err(illegal(pc, instruction));
        }
        let round = self.float_rounding((instruction >> 12) & 0b111, pc, instruction)?;
        let rd = ((instruction >> 7) & 0x1f) as usize;
        let rs1 = ((instruction >> 15) & 0x1f) as usize;
        let rs2 = ((instruction >> 20) & 0x1f) as usize;
        let rs3 = ((instruction >> 27) & 0x1f) as usize;
        let negate_product = matches!(opcode, 0x4b | 0x4f);
        let negate_addend = matches!(opcode, 0x47 | 0x4f);

        if format == 0 {
            let mut lhs = Single::from_bits(u128::from(self.cpu.read_float32(rs1)));
            let rhs = Single::from_bits(u128::from(self.cpu.read_float32(rs2)));
            let mut addend = Single::from_bits(u128::from(self.cpu.read_float32(rs3)));
            if negate_product {
                lhs = -lhs;
            }
            if negate_addend {
                addend = -addend;
            }
            let result = lhs.mul_add_r(rhs, addend, round);
            self.finish_float32(rd, result);
        } else {
            let mut lhs = Double::from_bits(u128::from(self.cpu.read_float64(rs1)));
            let rhs = Double::from_bits(u128::from(self.cpu.read_float64(rs2)));
            let mut addend = Double::from_bits(u128::from(self.cpu.read_float64(rs3)));
            if negate_product {
                lhs = -lhs;
            }
            if negate_addend {
                addend = -addend;
            }
            let result = lhs.mul_add_r(rhs, addend, round);
            self.finish_float64(rd, result);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn execute_float_op(
        &mut self,
        instruction: u32,
        rd: usize,
        rs1: usize,
        rs2: usize,
        funct3: u32,
        funct7: u32,
        pc: u64,
    ) -> Result<(), MachineTrap> {
        self.ensure_floating_enabled(pc, instruction)?;
        match funct7 {
            0x00 | 0x04 | 0x08 | 0x0c => {
                let round = self.float_rounding(funct3, pc, instruction)?;
                let lhs = Single::from_bits(u128::from(self.cpu.read_float32(rs1)));
                let rhs = Single::from_bits(u128::from(self.cpu.read_float32(rs2)));
                let result = match funct7 {
                    0x00 => lhs.add_r(rhs, round),
                    0x04 => lhs.sub_r(rhs, round),
                    0x08 => lhs.mul_r(rhs, round),
                    0x0c => lhs.div_r(rhs, round),
                    _ => unreachable!(),
                };
                self.finish_float32(rd, result);
            }
            0x01 | 0x05 | 0x09 | 0x0d => {
                let round = self.float_rounding(funct3, pc, instruction)?;
                let lhs = Double::from_bits(u128::from(self.cpu.read_float64(rs1)));
                let rhs = Double::from_bits(u128::from(self.cpu.read_float64(rs2)));
                let result = match funct7 {
                    0x01 => lhs.add_r(rhs, round),
                    0x05 => lhs.sub_r(rhs, round),
                    0x09 => lhs.mul_r(rhs, round),
                    0x0d => lhs.div_r(rhs, round),
                    _ => unreachable!(),
                };
                self.finish_float64(rd, result);
            }
            0x2c if rs2 == 0 => {
                let round = self.float_rounding(funct3, pc, instruction)?;
                let (result, _) = ieee_apsqrt::sqrt_accurate(self.cpu.read_float32(rs1), round);
                self.finish_float32_bits(rd, result);
            }
            0x2d if rs2 == 0 => {
                let round = self.float_rounding(funct3, pc, instruction)?;
                let (result, _) = ieee_apsqrt::sqrt_accurate(self.cpu.read_float64(rs1), round);
                self.finish_float64_bits(rd, result);
            }
            0x10 | 0x11 if funct3 <= 2 => {
                if funct7 == 0x10 {
                    let lhs = self.cpu.read_float32(rs1);
                    let rhs = self.cpu.read_float32(rs2);
                    let sign = float_sign_injection(u64::from(lhs), u64::from(rhs), 31, funct3);
                    self.cpu.write_float32(rd, sign as u32);
                } else {
                    let lhs = self.cpu.read_float64(rs1);
                    let rhs = self.cpu.read_float64(rs2);
                    self.cpu
                        .write_float64(rd, float_sign_injection(lhs, rhs, 63, funct3));
                }
                self.csrs.mark_float_dirty();
            }
            0x14 | 0x15 if funct3 <= 1 => {
                if funct7 == 0x14 {
                    let result = self.float_min_max32(
                        self.cpu.read_float32(rs1),
                        self.cpu.read_float32(rs2),
                        funct3 == 1,
                    );
                    self.cpu.write_float32(rd, result);
                } else {
                    let result = self.float_min_max64(
                        self.cpu.read_float64(rs1),
                        self.cpu.read_float64(rs2),
                        funct3 == 1,
                    );
                    self.cpu.write_float64(rd, result);
                }
                self.csrs.mark_float_dirty();
            }
            0x20 if rs2 == 1 => {
                let round = self.float_rounding(funct3, pc, instruction)?;
                let value = Double::from_bits(u128::from(self.cpu.read_float64(rs1)));
                let mut loses_info = false;
                let result: StatusAnd<Single> = value.convert_r(round, &mut loses_info);
                self.finish_float32(rd, result);
            }
            0x21 if rs2 == 0 => {
                let round = self.float_rounding(funct3, pc, instruction)?;
                let value = Single::from_bits(u128::from(self.cpu.read_float32(rs1)));
                let mut loses_info = false;
                let result: StatusAnd<Double> = value.convert_r(round, &mut loses_info);
                self.finish_float64(rd, result);
            }
            0x50 | 0x51 if funct3 <= 2 => {
                let result = if funct7 == 0x50 {
                    float_compare32(
                        self.cpu.read_float32(rs1),
                        self.cpu.read_float32(rs2),
                        funct3,
                    )
                } else {
                    float_compare64(
                        self.cpu.read_float64(rs1),
                        self.cpu.read_float64(rs2),
                        funct3,
                    )
                };
                let (value, flags) = result?;
                self.cpu.write(rd, u64::from(value));
                self.csrs.accrue_float_flags(flags);
            }
            0x60 | 0x61 if rs2 <= 3 => {
                let round = self.float_rounding(funct3, pc, instruction)?;
                let (value, status) = if funct7 == 0x60 {
                    float_to_integer(
                        Single::from_bits(u128::from(self.cpu.read_float32(rs1))),
                        rs2,
                        round,
                    )
                } else {
                    float_to_integer(
                        Double::from_bits(u128::from(self.cpu.read_float64(rs1))),
                        rs2,
                        round,
                    )
                };
                self.cpu.write(rd, value);
                self.csrs.accrue_float_flags(float_status_flags(status));
            }
            0x68 | 0x69 if rs2 <= 3 => {
                let round = self.float_rounding(funct3, pc, instruction)?;
                let signed = rs2 & 1 == 0;
                let width = if rs2 < 2 { 32 } else { 64 };
                if funct7 == 0x68 {
                    let result = integer_to_float32(self.cpu.read(rs1), signed, width, round);
                    self.finish_float32(rd, result);
                } else {
                    let result = integer_to_float64(self.cpu.read(rs1), signed, width, round);
                    self.finish_float64(rd, result);
                }
            }
            0x70 if rs2 == 0 && funct3 == 0 => {
                let value = sign_extend(self.cpu.floating_registers[rs1] & u64::from(u32::MAX), 32);
                self.cpu.write(rd, value);
                self.csrs.mark_float_dirty();
            }
            0x71 if rs2 == 0 && funct3 == 0 => {
                let value = self.cpu.read_float64(rs1);
                self.cpu.write(rd, value);
                self.csrs.mark_float_dirty();
            }
            0x70 if rs2 == 0 && funct3 == 1 => {
                let value = u64::from(float_class32(self.cpu.read_float32(rs1)));
                self.cpu.write(rd, value);
                self.csrs.mark_float_dirty();
            }
            0x71 if rs2 == 0 && funct3 == 1 => {
                let value = u64::from(float_class64(self.cpu.read_float64(rs1)));
                self.cpu.write(rd, value);
                self.csrs.mark_float_dirty();
            }
            0x78 if rs2 == 0 && funct3 == 0 => {
                let value = self.cpu.read(rs1) as u32;
                self.cpu.write_float32(rd, value);
                self.csrs.mark_float_dirty();
            }
            0x79 if rs2 == 0 && funct3 == 0 => {
                let value = self.cpu.read(rs1);
                self.cpu.write_float64(rd, value);
                self.csrs.mark_float_dirty();
            }
            _ => return Err(illegal(pc, instruction)),
        }
        Ok(())
    }

    fn ensure_floating_enabled(&self, pc: u64, instruction: u32) -> Result<(), MachineTrap> {
        if self.csrs.floating_enabled() {
            Ok(())
        } else {
            Err(illegal(pc, instruction))
        }
    }

    fn float_rounding(
        &self,
        encoded: u32,
        pc: u64,
        instruction: u32,
    ) -> Result<Round, MachineTrap> {
        let resolved = if encoded == 0b111 {
            u32::from((self.csrs.fcsr & FRM_MASK) >> FRM_SHIFT)
        } else {
            encoded
        };
        match resolved {
            0b000 => Ok(Round::NearestTiesToEven),
            0b001 => Ok(Round::TowardZero),
            0b010 => Ok(Round::TowardNegative),
            0b011 => Ok(Round::TowardPositive),
            0b100 => Ok(Round::NearestTiesToAway),
            _ => Err(illegal(pc, instruction)),
        }
    }

    fn finish_float32(&mut self, rd: usize, result: StatusAnd<Single>) {
        self.finish_float32_bits(
            rd,
            StatusAnd {
                status: result.status,
                value: result.value.to_bits() as u32,
            },
        );
    }

    fn finish_float64(&mut self, rd: usize, result: StatusAnd<Double>) {
        self.finish_float64_bits(
            rd,
            StatusAnd {
                status: result.status,
                value: result.value.to_bits() as u64,
            },
        );
    }

    fn finish_float32_bits(&mut self, rd: usize, result: StatusAnd<u32>) {
        let value = if float_is_nan32(result.value) {
            CANONICAL_NAN_F32
        } else {
            result.value
        };
        self.cpu.write_float32(rd, value);
        self.csrs
            .accrue_float_flags(float_status_flags(result.status));
    }

    fn finish_float64_bits(&mut self, rd: usize, result: StatusAnd<u64>) {
        let value = if float_is_nan64(result.value) {
            CANONICAL_NAN_F64
        } else {
            result.value
        };
        self.cpu.write_float64(rd, value);
        self.csrs
            .accrue_float_flags(float_status_flags(result.status));
    }

    fn float_min_max32(&mut self, lhs: u32, rhs: u32, maximum: bool) -> u32 {
        if float_is_signaling_nan32(lhs) || float_is_signaling_nan32(rhs) {
            self.csrs.accrue_float_flags(FFLAGS_NV);
        }
        match (float_is_nan32(lhs), float_is_nan32(rhs)) {
            (true, true) => CANONICAL_NAN_F32,
            (true, false) => rhs,
            (false, true) => lhs,
            (false, false) if float_is_zero32(lhs) && float_is_zero32(rhs) => {
                if maximum {
                    (lhs & rhs) & (1 << 31)
                } else {
                    (lhs | rhs) & (1 << 31)
                }
            }
            (false, false) => {
                let lhs_float = Single::from_bits(u128::from(lhs));
                let rhs_float = Single::from_bits(u128::from(rhs));
                let order = lhs_float
                    .partial_cmp(&rhs_float)
                    .expect("non-NaN floats compare");
                if (maximum && order == Ordering::Less) || (!maximum && order == Ordering::Greater)
                {
                    rhs
                } else {
                    lhs
                }
            }
        }
    }

    fn float_min_max64(&mut self, lhs: u64, rhs: u64, maximum: bool) -> u64 {
        if float_is_signaling_nan64(lhs) || float_is_signaling_nan64(rhs) {
            self.csrs.accrue_float_flags(FFLAGS_NV);
        }
        match (float_is_nan64(lhs), float_is_nan64(rhs)) {
            (true, true) => CANONICAL_NAN_F64,
            (true, false) => rhs,
            (false, true) => lhs,
            (false, false) if float_is_zero64(lhs) && float_is_zero64(rhs) => {
                if maximum {
                    (lhs & rhs) & (1 << 63)
                } else {
                    (lhs | rhs) & (1 << 63)
                }
            }
            (false, false) => {
                let lhs_float = Double::from_bits(u128::from(lhs));
                let rhs_float = Double::from_bits(u128::from(rhs));
                let order = lhs_float
                    .partial_cmp(&rhs_float)
                    .expect("non-NaN floats compare");
                if (maximum && order == Ordering::Less) || (!maximum && order == Ordering::Greater)
                {
                    rhs
                } else {
                    lhs
                }
            }
        }
    }
}

fn float_status_flags(status: FloatStatus) -> u8 {
    let mut flags = 0;
    if status.contains(FloatStatus::INVALID_OP) {
        flags |= FFLAGS_NV;
    }
    if status.contains(FloatStatus::DIV_BY_ZERO) {
        flags |= FFLAGS_DZ;
    }
    if status.contains(FloatStatus::OVERFLOW) {
        flags |= FFLAGS_OF;
    }
    if status.contains(FloatStatus::UNDERFLOW) {
        flags |= FFLAGS_UF;
    }
    if status.contains(FloatStatus::INEXACT) {
        flags |= FFLAGS_NX;
    }
    flags
}

fn float_sign_injection(lhs: u64, rhs: u64, sign_bit: u32, operation: u32) -> u64 {
    let sign_mask = 1_u64 << sign_bit;
    let rhs_sign = rhs & sign_mask;
    let sign = match operation {
        0 => rhs_sign,
        1 => rhs_sign ^ sign_mask,
        2 => (lhs ^ rhs) & sign_mask,
        _ => unreachable!(),
    };
    (lhs & !sign_mask) | sign
}

fn float_compare32(lhs: u32, rhs: u32, operation: u32) -> Result<(bool, u8), MachineTrap> {
    let nan = float_is_nan32(lhs) || float_is_nan32(rhs);
    let signaling = float_is_signaling_nan32(lhs) || float_is_signaling_nan32(rhs);
    if nan {
        return Ok((
            false,
            if operation == 2 && !signaling {
                0
            } else {
                FFLAGS_NV
            },
        ));
    }
    let lhs = Single::from_bits(u128::from(lhs));
    let rhs = Single::from_bits(u128::from(rhs));
    Ok((
        match operation {
            0 => lhs <= rhs,
            1 => lhs < rhs,
            2 => lhs == rhs,
            _ => unreachable!(),
        },
        0,
    ))
}

fn float_compare64(lhs: u64, rhs: u64, operation: u32) -> Result<(bool, u8), MachineTrap> {
    let nan = float_is_nan64(lhs) || float_is_nan64(rhs);
    let signaling = float_is_signaling_nan64(lhs) || float_is_signaling_nan64(rhs);
    if nan {
        return Ok((
            false,
            if operation == 2 && !signaling {
                0
            } else {
                FFLAGS_NV
            },
        ));
    }
    let lhs = Double::from_bits(u128::from(lhs));
    let rhs = Double::from_bits(u128::from(rhs));
    Ok((
        match operation {
            0 => lhs <= rhs,
            1 => lhs < rhs,
            2 => lhs == rhs,
            _ => unreachable!(),
        },
        0,
    ))
}

fn float_to_integer<F: Float>(value: F, kind: usize, round: Round) -> (u64, FloatStatus) {
    let signed = kind & 1 == 0;
    let width = if kind < 2 { 32 } else { 64 };
    let mut exact = false;
    let result = if signed {
        if value.is_nan() {
            StatusAnd {
                status: FloatStatus::INVALID_OP,
                value: (1_i128 << (width - 1)) - 1,
            }
        } else {
            value.to_i128_r(width, round, &mut exact)
        }
        .map(|integer| integer as u128)
    } else if value.is_nan() {
        StatusAnd {
            status: FloatStatus::INVALID_OP,
            value: (1_u128 << width) - 1,
        }
    } else {
        value.to_u128_r(width, round, &mut exact)
    };
    let value = result.value as u64;
    (
        if width == 32 {
            sign_extend(value & u64::from(u32::MAX), 32)
        } else {
            value
        },
        result.status,
    )
}

fn integer_to_float32(value: u64, signed: bool, width: usize, round: Round) -> StatusAnd<Single> {
    if signed {
        let value = if width == 32 {
            i128::from(value as u32 as i32)
        } else {
            i128::from(value as i64)
        };
        Single::from_i128_r(value, round)
    } else {
        let value = if width == 32 {
            u128::from(value as u32)
        } else {
            u128::from(value)
        };
        Single::from_u128_r(value, round)
    }
}

fn integer_to_float64(value: u64, signed: bool, width: usize, round: Round) -> StatusAnd<Double> {
    if signed {
        let value = if width == 32 {
            i128::from(value as u32 as i32)
        } else {
            i128::from(value as i64)
        };
        Double::from_i128_r(value, round)
    } else {
        let value = if width == 32 {
            u128::from(value as u32)
        } else {
            u128::from(value)
        };
        Double::from_u128_r(value, round)
    }
}

fn float_is_nan32(value: u32) -> bool {
    value & 0x7fff_ffff > 0x7f80_0000
}

fn float_is_signaling_nan32(value: u32) -> bool {
    float_is_nan32(value) && value & (1 << 22) == 0
}

fn float_is_zero32(value: u32) -> bool {
    value & 0x7fff_ffff == 0
}

fn float_is_nan64(value: u64) -> bool {
    value & 0x7fff_ffff_ffff_ffff > 0x7ff0_0000_0000_0000
}

fn float_is_signaling_nan64(value: u64) -> bool {
    float_is_nan64(value) && value & (1 << 51) == 0
}

fn float_is_zero64(value: u64) -> bool {
    value & 0x7fff_ffff_ffff_ffff == 0
}

fn float_class32(value: u32) -> u16 {
    let sign = value >> 31 != 0;
    let exponent = value >> 23 & 0xff;
    let fraction = value & 0x7f_ffff;
    match (sign, exponent, fraction) {
        (true, 0xff, 0) => 1 << 0,
        (true, 1..=0xfe, _) => 1 << 1,
        (true, 0, 1..) => 1 << 2,
        (true, 0, 0) => 1 << 3,
        (false, 0, 0) => 1 << 4,
        (false, 0, 1..) => 1 << 5,
        (false, 1..=0xfe, _) => 1 << 6,
        (false, 0xff, 0) => 1 << 7,
        (_, 0xff, _) if float_is_signaling_nan32(value) => 1 << 8,
        (_, 0xff, _) => 1 << 9,
        _ => unreachable!(),
    }
}

fn float_class64(value: u64) -> u16 {
    let sign = value >> 63 != 0;
    let exponent = value >> 52 & 0x7ff;
    let fraction = value & 0x000f_ffff_ffff_ffff;
    match (sign, exponent, fraction) {
        (true, 0x7ff, 0) => 1 << 0,
        (true, 1..=0x7fe, _) => 1 << 1,
        (true, 0, 1..) => 1 << 2,
        (true, 0, 0) => 1 << 3,
        (false, 0, 0) => 1 << 4,
        (false, 0, 1..) => 1 << 5,
        (false, 1..=0x7fe, _) => 1 << 6,
        (false, 0x7ff, 0) => 1 << 7,
        (_, 0x7ff, _) if float_is_signaling_nan64(value) => 1 << 8,
        (_, 0x7ff, _) => 1 << 9,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping_and_nan_classification_match_riscv_bits() {
        assert_eq!(float_status_flags(FloatStatus::INVALID_OP), FFLAGS_NV);
        assert_eq!(float_status_flags(FloatStatus::DIV_BY_ZERO), FFLAGS_DZ);
        assert_eq!(float_status_flags(FloatStatus::OVERFLOW), FFLAGS_OF);
        assert_eq!(float_status_flags(FloatStatus::UNDERFLOW), FFLAGS_UF);
        assert_eq!(float_status_flags(FloatStatus::INEXACT), FFLAGS_NX);
        assert_eq!(float_class32(0x7f80_0001), 1 << 8);
        assert_eq!(float_class32(CANONICAL_NAN_F32), 1 << 9);
        assert_eq!(float_class64(0xfff0_0000_0000_0000), 1 << 0);
        assert_eq!(float_class64(1), 1 << 5);
    }

    #[test]
    fn integer_conversion_uses_riscv_nan_and_word_results() {
        let nan = Single::from_bits(u128::from(CANONICAL_NAN_F32));
        let (signed, status) = float_to_integer(nan, 0, Round::NearestTiesToEven);
        assert_eq!(signed, 0x0000_0000_7fff_ffff);
        assert_eq!(status, FloatStatus::INVALID_OP);
        let (unsigned, status) = float_to_integer(nan, 1, Round::NearestTiesToEven);
        assert_eq!(unsigned, u64::MAX);
        assert_eq!(status, FloatStatus::INVALID_OP);
    }
}
