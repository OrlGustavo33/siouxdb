//! CRC32C (Castagnoli), o mesmo usado por iSCSI, ext4 e outros bancos.
//! Usa a instrução da CPU (SSE4.2 no x86, CRC no ARMv8) quando existe, senão tabelas
//! por software (crate `crc32c`).

/// CRC32C dos dados.
pub fn crc32c(dados: &[u8]) -> u32 {
    ::crc32c::crc32c(dados)
}

/// Continua um CRC32C: `crc32c_continuar(crc32c(a), b) == crc32c(a ++ b)`.
/// Usado para encadear os quadros do WAL.
pub fn crc32c_continuar(crc: u32, dados: &[u8]) -> u32 {
    ::crc32c::crc32c_append(crc, dados)
}

#[cfg(test)]
mod testes {
    use super::{crc32c, crc32c_continuar};

    #[test]
    fn vetores_conhecidos() {
        // Vetor padrão de conferência do CRC-32C.
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b""), 0);
        // RFC 3720 (iSCSI): 32 bytes zerados.
        assert_eq!(crc32c(&[0u8; 32]), 0x8A91_36AA);
        assert_eq!(
            crc32c_continuar(crc32c(b"12345"), b"6789"),
            crc32c(b"123456789")
        );
    }

    #[test]
    fn igual_a_implementacao_por_tabela() {
        // A tabela byte a byte de antes (formato 1): qualquer tamanho, qualquer ponto de corte.
        fn por_tabela(crc: u32, dados: &[u8]) -> u32 {
            let mut c = !crc;
            for &b in dados {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        (c >> 1) ^ 0x82F6_3B78
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        let dados: Vec<u8> = (0..20_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        for corte in [0, 1, 7, 8, 63, 4096, 8191, 19_999, 20_000] {
            let a = &dados[..corte];
            let b = &dados[corte..];
            assert_eq!(crc32c(a), por_tabela(0, a));
            assert_eq!(
                crc32c_continuar(crc32c(a), b),
                por_tabela(por_tabela(0, a), b)
            );
        }
    }
}
