use super::scalar::Scalar;
use super::transcript::ProofTranscript;
use ff::Field;
use merlin::Transcript;
use rand_core::OsRng;

pub struct RandomTape {
  tape: Transcript,
}

impl RandomTape {
  pub fn new(name: &'static [u8]) -> Self {
    let tape = {
      let mut csprng: OsRng = OsRng;
      let mut tape = Transcript::new(name);
      tape.append_scalar(b"init_randomness", &Scalar::random(&mut csprng));
      tape
    };
    Self { tape }
  }

  pub fn random_scalar(&mut self, label: &'static [u8]) -> Scalar {
    self.tape.challenge_scalar(label)
  }
}
