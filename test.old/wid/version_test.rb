module Wid
  class VersionTest < Test
    def test_version
      refute_nil Wid::VERSION
    end
  end
end
